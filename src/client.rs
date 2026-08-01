use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};

use crate::model::{
    Access, CatalogError, Constraint, DeviceCatalog, DisplayValue, Menu, MenuGroup, Parameter,
    ParameterIdentity, ParameterOid, ParameterSelector, ParameterValue, ResolveError, SetSemantics,
};
use crate::protocol::{
    self, CLIENT_ADDRESS, ExternalObjectFragment, Frame, FrameDecoder, HandshakeResponse,
    MenuCount, MenuDescription, MenuGroupName, MenuNumericOids, MenuStringOidPage, NumericOidPage,
    ParameterCounts, ParameterResponse, ProtocolError, REPORT_PARAM, REPORT_STRING, RESPONSE_BIT,
    RawDescriptor, StringOidPage,
};
use crate::value::{self, ValueError};

const OGP_PORT: u16 = 5253;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const ATTEMPTS: usize = 3;
const TRIGGER_SET_ATTEMPTS: usize = 1;
const OID_PAGE_SIZE: usize = 128;
const MAX_PARAMETERS: usize = 100_000;
const MAX_EXTERNAL_OBJECT_SIZE: usize = 1024 * 1024;
const MAX_EXTERNAL_FRAGMENTS: usize = 4096;

#[derive(Debug)]
pub struct OgpClient {
    stream: TcpStream,
    decoder: FrameDecoder,
    slot: Slot,
    catalog: Option<DeviceCatalog>,
}

impl OgpClient {
    pub async fn connect(host: &str, slot: Slot, force: bool) -> Result<Self, ClientError> {
        Self::connect_with_port(host, OGP_PORT, slot, force).await
    }

    pub async fn connect_with_port(
        host: &str,
        port: u16,
        slot: Slot,
        force: bool,
    ) -> Result<Self, ClientError> {
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| ClientError::Timeout { phase: "connect" })??;
        stream.set_nodelay(true)?;
        let mut client = Self {
            stream,
            decoder: FrameDecoder::default(),
            slot,
            catalog: None,
        };
        client.handshake(force).await?;
        Ok(client)
    }

    pub async fn discover(&mut self) -> Result<&DeviceCatalog, ClientError> {
        if self.catalog.is_none() {
            let catalog = self.discover_catalog().await?;
            self.catalog = Some(catalog);
        }
        Ok(self.catalog.as_ref().expect("catalog was initialized"))
    }

    pub fn catalog(&self) -> Option<&DeviceCatalog> {
        self.catalog.as_ref()
    }

    pub async fn describe_parameter(
        &mut self,
        oid: &ParameterOid,
    ) -> Result<Parameter, ClientError> {
        self.fetch_parameter(oid, &mut HashMap::new()).await
    }

    pub async fn read(&mut self, selector: &ParameterSelector) -> Result<ReadResult, ClientError> {
        let parameter = self.discover().await?.resolve(selector)?.clone();
        self.read_parameter(&parameter).await
    }

    pub async fn read_parameter(
        &mut self,
        parameter: &Parameter,
    ) -> Result<ReadResult, ClientError> {
        if !parameter.is_runtime_supported() {
            return Err(ClientError::UnsupportedType(parameter.identity()));
        }
        let request = protocol::get_request(self.slot.address(), &parameter.oid)?;
        let frame = self
            .exchange(
                request,
                ATTEMPTS,
                Correlation::Parameter(parameter.oid.clone()),
            )
            .await?;
        let response =
            ParameterResponse::parse(&frame, &parameter.oid).map_err(ClientError::from_protocol)?;
        let value = value::decode(parameter, &response.value).map_err(|source| {
            ClientError::InvalidParameterValue {
                parameter: parameter.identity(),
                bytes: response
                    .value
                    .iter()
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                source,
            }
        })?;
        Ok(ReadResult {
            parameter: parameter.identity(),
            display_value: parameter.interpret(&value),
            value,
        })
    }

    pub async fn write(
        &mut self,
        selector: &ParameterSelector,
        value: ParameterValue,
    ) -> Result<WriteResult, ClientError> {
        let parameter = self.discover().await?.resolve(selector)?.clone();
        self.write_parameter(&parameter, value).await
    }

    pub async fn write_parameter(
        &mut self,
        parameter: &Parameter,
        value: ParameterValue,
    ) -> Result<WriteResult, ClientError> {
        if parameter.access == Access::ReadOnly {
            return Err(ClientError::ReadOnly(parameter.identity()));
        }
        let encoded = value::encode(parameter, &value)?;
        let request = protocol::set_request(self.slot.address(), &parameter.oid, &encoded)?;
        let attempts = match parameter.set_semantics {
            SetSemantics::Value => ATTEMPTS,
            SetSemantics::Trigger => TRIGGER_SET_ATTEMPTS,
        };
        let frame = self
            .exchange(
                request,
                attempts,
                Correlation::Parameter(parameter.oid.clone()),
            )
            .await?;
        let response =
            ParameterResponse::parse(&frame, &parameter.oid).map_err(ClientError::from_protocol)?;
        let actual = value::decode(parameter, &response.value)?;
        Ok(WriteResult {
            parameter: parameter.identity(),
            display_value: parameter.interpret(&actual),
            requested_value: value,
            value: actual,
        })
    }

    async fn discover_catalog(&mut self) -> Result<DeviceCatalog, ClientError> {
        let counts = self.parameter_counts().await?;
        let string_count = usize::try_from(counts.string)
            .map_err(|_| ClientError::InvalidDiscovery("string OID count exceeds this platform"))?;
        let total = (counts.numeric as usize).checked_add(string_count).ok_or(
            ClientError::InvalidDiscovery("device parameter count exceeds this platform"),
        )?;
        if total > MAX_PARAMETERS {
            return Err(ClientError::InvalidDiscovery(
                "device parameter count exceeds the 100,000 parameter safety limit",
            ));
        }
        let mut oids = self.numeric_oids(counts.numeric).await?;
        oids.extend(self.string_oids(counts.string).await?);

        let mut external_objects: HashMap<u16, Vec<u8>> = HashMap::new();
        let mut parameters = Vec::with_capacity(oids.len());
        for oid in oids {
            parameters.push(self.fetch_parameter(&oid, &mut external_objects).await?);
        }
        let mut catalog = DeviceCatalog::new(parameters)?;
        let (menus, error, incomplete_groups) = self.discover_menus().await;
        catalog.set_menus(menus, error, incomplete_groups);
        Ok(catalog)
    }

    async fn fetch_parameter(
        &mut self,
        oid: &ParameterOid,
        external_objects: &mut HashMap<u16, Vec<u8>>,
    ) -> Result<Parameter, ClientError> {
        let request = protocol::descriptor_request(self.slot.address(), oid)?;
        let frame = self
            .exchange(request, ATTEMPTS, Correlation::Parameter(oid.clone()))
            .await?;
        let descriptor = RawDescriptor::parse(&frame, oid).map_err(ClientError::from_protocol)?;
        let constraint = match descriptor.inline_constraint()? {
            Constraint::External { object_id, .. } => {
                let bytes = if let Some(bytes) = external_objects.get(&object_id) {
                    bytes.clone()
                } else {
                    let bytes = self.external_constraint_object(object_id).await?;
                    external_objects.insert(object_id, bytes.clone());
                    bytes
                };
                let resolved = protocol::external_constraint(&bytes, &descriptor.parameter_type())?;
                Constraint::External {
                    object_id,
                    resolved: Box::new(resolved),
                }
            }
            constraint => constraint,
        };
        descriptor
            .into_parameter(constraint)
            .map_err(ClientError::from_protocol)
    }

    async fn discover_menus(&mut self) -> (Vec<MenuGroup>, Option<String>, Vec<u8>) {
        let mut groups = Vec::with_capacity(2);
        let mut errors = Vec::new();
        let mut incomplete_groups = Vec::new();
        for group_index in 0..=1 {
            match self.discover_menu_group(group_index).await {
                Ok((group, group_errors)) => {
                    if !group_errors.is_empty() {
                        incomplete_groups.push(group_index);
                    }
                    groups.push(group);
                    errors.extend(group_errors);
                }
                Err(error) => {
                    incomplete_groups.push(group_index);
                    errors.push(format!("menu group {group_index}: {error}"));
                }
            }
        }
        let error = (!errors.is_empty()).then(|| errors.join("; "));
        (groups, error, incomplete_groups)
    }

    async fn discover_menu_group(
        &mut self,
        group_index: u8,
    ) -> Result<(MenuGroup, Vec<String>), ClientError> {
        let request = protocol::menu_group_request(
            self.slot.address(),
            protocol::GET_MENUSET_NAME,
            group_index,
        );
        let frame = self
            .exchange(request, ATTEMPTS, Correlation::MenuGroup(group_index))
            .await?;
        let group_name = MenuGroupName::parse(&frame).map_err(ClientError::from_protocol)?;
        if group_name.group != group_index {
            return Err(ClientError::InvalidDiscovery(
                "menu group name response does not match its request",
            ));
        }

        let request = protocol::menu_group_request(
            self.slot.address(),
            protocol::GET_MENU_COUNT,
            group_index,
        );
        let frame = self
            .exchange(request, ATTEMPTS, Correlation::MenuGroup(group_index))
            .await?;
        let count = MenuCount::parse(&frame).map_err(ClientError::from_protocol)?;
        if count.group != group_index {
            return Err(ClientError::InvalidDiscovery(
                "menu count response does not match its request",
            ));
        }

        let mut menus = Vec::with_capacity(count.count as usize);
        let mut errors = Vec::new();
        for menu_index in 0..count.count {
            match self.discover_menu(group_index, menu_index).await {
                Ok(menu) => menus.push(menu),
                Err(error) => errors.push(format!(
                    "menu group {group_index}, menu {menu_index}: {error}"
                )),
            }
        }
        Ok((
            MenuGroup {
                index: group_index,
                name: group_name.name,
                menus,
            },
            errors,
        ))
    }

    async fn discover_menu(
        &mut self,
        group_index: u8,
        menu_index: u8,
    ) -> Result<Menu, ClientError> {
        let request = protocol::menu_request(
            self.slot.address(),
            protocol::GET_MENU_NAME,
            group_index,
            menu_index,
        );
        let frame = self
            .exchange(
                request,
                ATTEMPTS,
                Correlation::Menu(group_index, menu_index),
            )
            .await?;
        let description = MenuDescription::parse(&frame).map_err(ClientError::from_protocol)?;
        if description.group != group_index || description.menu != menu_index {
            return Err(ClientError::InvalidDiscovery(
                "menu name response does not match its request",
            ));
        }
        let members = if let Some(string_count) =
            description.string_oid_count.filter(|count| *count > 0)
        {
            self.menu_string_oids(group_index, menu_index, string_count)
                .await?
        } else {
            let request = protocol::menu_request(
                self.slot.address(),
                protocol::GET_MENU_OIDS,
                group_index,
                menu_index,
            );
            let frame = self
                .exchange(
                    request,
                    ATTEMPTS,
                    Correlation::Menu(group_index, menu_index),
                )
                .await?;
            let response = MenuNumericOids::parse(&frame).map_err(ClientError::from_protocol)?;
            if response.group != group_index || response.menu != menu_index {
                return Err(ClientError::InvalidDiscovery(
                    "menu OID response does not match its request",
                ));
            }
            response
                .oids
                .into_iter()
                .map(ParameterOid::Numeric)
                .collect()
        };
        Ok(Menu {
            index: menu_index,
            name: description.name,
            stable_id: description
                .stable_id
                .unwrap_or((u16::from(group_index) << 8) | u16::from(menu_index)),
            layout_url: description.layout_url,
            members,
        })
    }

    async fn menu_string_oids(
        &mut self,
        group: u8,
        menu: u8,
        count: u16,
    ) -> Result<Vec<ParameterOid>, ClientError> {
        let mut result = Vec::with_capacity(count as usize);
        while result.len() < count as usize {
            let first = u16::try_from(result.len()).expect("menu string OID count is u16");
            let requested = (count as usize - result.len()).min(OID_PAGE_SIZE) as u16;
            let request = protocol::menu_string_oid_page_request(
                self.slot.address(),
                group,
                menu,
                first,
                requested,
            );
            let frame = self
                .exchange(
                    request,
                    ATTEMPTS,
                    Correlation::MenuStringPage { group, menu, first },
                )
                .await?;
            let page = MenuStringOidPage::parse(&frame).map_err(ClientError::from_protocol)?;
            if page.group != group
                || page.menu != menu
                || page.first != first
                || page.oids.is_empty()
                || page.oids.len() > requested as usize
            {
                return Err(ClientError::InvalidDiscovery(
                    "menu string OID page does not match its request",
                ));
            }
            for oid in page.oids {
                let oid = oid.parse::<ParameterOid>().map_err(|_| {
                    ClientError::InvalidDiscovery("device returned an invalid menu string OID")
                })?;
                if !matches!(oid, ParameterOid::String(_)) {
                    return Err(ClientError::InvalidDiscovery(
                        "device returned a numeric OID in a string menu",
                    ));
                }
                result.push(oid);
            }
        }
        Ok(result)
    }

    async fn parameter_counts(&mut self) -> Result<ParameterCounts, ClientError> {
        let modern = protocol::num_params_request(self.slot.address(), true);
        let frame = self.exchange(modern, ATTEMPTS, Correlation::None).await?;
        match ParameterCounts::parse(&frame) {
            Ok(counts) => Ok(counts),
            Err(ProtocolError::Remote(0x01 | 0x02)) => {
                let legacy = protocol::num_params_request(self.slot.address(), false);
                let frame = self.exchange(legacy, ATTEMPTS, Correlation::None).await?;
                ParameterCounts::parse(&frame).map_err(ClientError::from_protocol)
            }
            Err(error) => Err(ClientError::from_protocol(error)),
        }
    }

    async fn numeric_oids(&mut self, count: u16) -> Result<Vec<ParameterOid>, ClientError> {
        let mut result = Vec::with_capacity(count as usize);
        while result.len() < count as usize {
            let first = u16::try_from(result.len()).expect("numeric parameter count is u16");
            let requested = (count as usize - result.len()).min(OID_PAGE_SIZE) as u8;
            let request = protocol::numeric_oid_page_request(self.slot.address(), first, requested);
            let frame = self
                .exchange(request, ATTEMPTS, Correlation::NumericPage(first))
                .await?;
            let page = NumericOidPage::parse(&frame).map_err(ClientError::from_protocol)?;
            if page.first != first || page.oids.is_empty() || page.oids.len() > requested as usize {
                return Err(ClientError::InvalidDiscovery(
                    "device returned an invalid numeric OID page",
                ));
            }
            result.extend(page.oids.into_iter().map(ParameterOid::Numeric));
        }
        if result.len() != count as usize {
            return Err(ClientError::InvalidDiscovery(
                "numeric OID count does not match enumeration",
            ));
        }
        Ok(result)
    }

    async fn string_oids(&mut self, count: u32) -> Result<Vec<ParameterOid>, ClientError> {
        let capacity = usize::try_from(count)
            .map_err(|_| ClientError::InvalidDiscovery("string OID count exceeds this platform"))?;
        let mut result = Vec::with_capacity(capacity);
        while result.len() < capacity {
            let first =
                u32::try_from(result.len()).expect("string OID count was converted to usize");
            let requested = (capacity - result.len()).min(OID_PAGE_SIZE) as u16;
            let request = protocol::string_oid_page_request(self.slot.address(), first, requested);
            let frame = self
                .exchange(request, ATTEMPTS, Correlation::StringPage(first))
                .await?;
            let page = StringOidPage::parse(&frame).map_err(ClientError::from_protocol)?;
            if page.first != first || page.oids.is_empty() || page.oids.len() > requested as usize {
                return Err(ClientError::InvalidDiscovery(
                    "device returned an invalid string OID page",
                ));
            }
            for oid in page.oids {
                let oid = oid.parse::<ParameterOid>().map_err(|_| {
                    ClientError::InvalidDiscovery("device returned an invalid string OID")
                })?;
                if !matches!(oid, ParameterOid::String(_)) {
                    return Err(ClientError::InvalidDiscovery(
                        "device returned a numeric value in its string OID table",
                    ));
                }
                result.push(oid);
            }
        }
        if result.len() != capacity {
            return Err(ClientError::InvalidDiscovery(
                "string OID count does not match enumeration",
            ));
        }
        Ok(result)
    }

    async fn external_constraint_object(&mut self, object_id: u16) -> Result<Vec<u8>, ClientError> {
        let mut bytes = Vec::new();
        let mut requested_fragment = 0;
        let mut seen = HashSet::new();
        for _ in 0..MAX_EXTERNAL_FRAGMENTS {
            if !seen.insert(requested_fragment) {
                return Err(ClientError::InvalidDiscovery(
                    "external object contains a fragment loop",
                ));
            }
            let request = protocol::external_object_request(
                self.slot.address(),
                object_id,
                requested_fragment,
            );
            let frame = self
                .exchange(
                    request,
                    ATTEMPTS,
                    Correlation::ExternalObject {
                        oid: object_id,
                        fragment: requested_fragment,
                    },
                )
                .await?;
            let fragment =
                ExternalObjectFragment::parse(&frame).map_err(ClientError::from_protocol)?;
            if fragment.oid != object_id || fragment.fragment != requested_fragment {
                return Err(ClientError::InvalidDiscovery(
                    "external object response does not match its request",
                ));
            }
            if bytes.len() + fragment.data.len() > MAX_EXTERNAL_OBJECT_SIZE {
                return Err(ClientError::InvalidDiscovery(
                    "external object exceeds the 1 MiB safety limit",
                ));
            }
            bytes.extend_from_slice(&fragment.data);
            if bytes.len() >= 2 && u16::from_be_bytes([bytes[0], bytes[1]]) != 1 {
                return Ok(bytes);
            }
            if fragment.next == 0 {
                return Ok(bytes);
            }
            requested_fragment = fragment.next;
        }
        Err(ClientError::InvalidDiscovery(
            "external object exceeds the fragment safety limit",
        ))
    }

    async fn handshake(&mut self, force: bool) -> Result<(), ClientError> {
        let request = protocol::handshake_request(force);
        let frame = self
            .exchange(
                request,
                ATTEMPTS,
                Correlation::Parameter(ParameterOid::Numeric(protocol::CONNECTION_OID)),
            )
            .await?;
        let response = HandshakeResponse::parse(&frame).map_err(ClientError::from_protocol)?;
        if response.allowed {
            Ok(())
        } else {
            Err(ClientError::handshake_rejected(
                force,
                response.refusal_reason,
            ))
        }
    }

    async fn exchange(
        &mut self,
        request: Frame,
        attempts: usize,
        correlation: Correlation,
    ) -> Result<Frame, ClientError> {
        let expected_type = request.message_type | RESPONSE_BIT;
        let expected_source = request.destination;
        for _ in 0..attempts {
            self.send(&request).await?;
            let deadline = Instant::now() + REQUEST_TIMEOUT;
            loop {
                let frame = match self.receive_until(deadline).await {
                    Ok(frame) => frame,
                    Err(ClientError::Timeout { .. }) => break,
                    Err(error) => return Err(error),
                };
                if matches!(frame.message_type, REPORT_PARAM | REPORT_STRING) {
                    continue;
                }
                if frame.source == expected_source
                    && frame.destination == CLIENT_ADDRESS
                    && frame.message_type == expected_type
                    && correlation.matches(&frame)
                {
                    return Ok(frame);
                }
            }
        }
        Err(ClientError::Timeout { phase: "request" })
    }

    async fn send(&mut self, frame: &Frame) -> Result<(), ClientError> {
        let bytes = frame.encode()?;
        timeout(REQUEST_TIMEOUT, self.stream.write_all(&bytes))
            .await
            .map_err(|_| ClientError::Timeout { phase: "send" })??;
        Ok(())
    }

    async fn receive_until(&mut self, deadline: Instant) -> Result<Frame, ClientError> {
        loop {
            if let Some(frame) = self.decoder.next_frame()? {
                return Ok(frame);
            }
            let mut bytes = [0u8; 2048];
            let count = timeout_at(deadline, self.stream.read(&mut bytes))
                .await
                .map_err(|_| ClientError::Timeout { phase: "receive" })??;
            if count == 0 {
                return Err(ClientError::ConnectionClosed);
            }
            self.decoder.push(&bytes[..count]);
        }
    }
}

#[derive(Debug)]
enum Correlation {
    None,
    Parameter(ParameterOid),
    NumericPage(u16),
    StringPage(u32),
    ExternalObject { oid: u16, fragment: u16 },
    MenuGroup(u8),
    Menu(u8, u8),
    MenuStringPage { group: u8, menu: u8, first: u16 },
}

impl Correlation {
    fn matches(&self, frame: &Frame) -> bool {
        if frame.content.first().is_some_and(|code| *code != 0)
            && matches!(
                self,
                Self::MenuGroup(_) | Self::Menu(_, _) | Self::MenuStringPage { .. }
            )
        {
            return true;
        }
        match self {
            Self::None => true,
            Self::Parameter(ParameterOid::Numeric(expected)) => frame
                .content
                .get(1..3)
                .is_some_and(|bytes| bytes == expected.to_be_bytes()),
            Self::Parameter(ParameterOid::String(expected)) => {
                let Some(&length) = frame.content.get(1) else {
                    return false;
                };
                let length = length as usize;
                frame.content.get(2..2 + length).is_some_and(|bytes| {
                    bytes.last() == Some(&0)
                        && length > 0
                        && &bytes[..length - 1] == expected.as_bytes()
                })
            }
            Self::NumericPage(expected) => frame
                .content
                .get(1..3)
                .is_some_and(|bytes| bytes == expected.to_be_bytes()),
            Self::StringPage(expected) => frame
                .content
                .get(1..5)
                .is_some_and(|bytes| bytes == expected.to_be_bytes()),
            Self::ExternalObject { oid, fragment } => {
                frame
                    .content
                    .get(1..3)
                    .is_some_and(|bytes| bytes == oid.to_be_bytes())
                    && frame
                        .content
                        .get(3..5)
                        .is_some_and(|bytes| bytes == fragment.to_be_bytes())
            }
            Self::MenuGroup(group) => frame.content.get(1) == Some(group),
            Self::Menu(group, menu) => {
                frame.content.get(1) == Some(group) && frame.content.get(2) == Some(menu)
            }
            Self::MenuStringPage { group, menu, first } => {
                frame.content.get(1) == Some(group)
                    && frame.content.get(2) == Some(menu)
                    && frame
                        .content
                        .get(3..5)
                        .is_some_and(|bytes| bytes == first.to_be_bytes())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot(u8);

impl Slot {
    pub fn new(value: u8) -> Result<Self, InvalidSlot> {
        if (1..=20).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidSlot(value))
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }

    fn address(self) -> u8 {
        0x10 + self.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadResult {
    pub parameter: ParameterIdentity,
    pub value: ParameterValue,
    pub display_value: Option<DisplayValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteResult {
    pub parameter: ParameterIdentity,
    pub requested_value: ParameterValue,
    pub value: ParameterValue,
    pub display_value: Option<DisplayValue>,
}

#[derive(Debug, Error)]
#[error("slot must be between 1 and 20, received {0}")]
pub struct InvalidSlot(u8);

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("{phase} timed out")]
    Timeout { phase: &'static str },
    #[error("connection closed before a complete response was received")]
    ConnectionClosed,
    #[error("connection handshake was rejected (reason {reason:?})")]
    HandshakeRejected { reason: Option<u16> },
    #[error("connection refused because all available client connections are in use")]
    ConnectionInUse,
    #[error("connection requires authentication, which is not supported")]
    AuthenticationRequired,
    #[error("parameter is read-only: {} ({})", .0.oid, .0.display_name)]
    ReadOnly(ParameterIdentity),
    #[error("parameter type is not supported at runtime: {} ({})", .0.oid, .0.display_name)]
    UnsupportedType(ParameterIdentity),
    #[error("invalid value returned for {} ({}): {source}; bytes: {bytes}", parameter.oid, parameter.display_name)]
    InvalidParameterValue {
        parameter: ParameterIdentity,
        bytes: String,
        #[source]
        source: ValueError,
    },
    #[error("card returned OGP error {code:#04x}: {description}")]
    Remote { code: u8, description: &'static str },
    #[error("invalid device metadata: {0}")]
    InvalidDiscovery(&'static str),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Value(#[from] ValueError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ClientError {
    fn handshake_rejected(force: bool, reason: Option<u16>) -> Self {
        match reason {
            Some(0) if !force => Self::ConnectionInUse,
            Some(2) => Self::AuthenticationRequired,
            reason => Self::HandshakeRejected { reason },
        }
    }

    fn from_protocol(error: ProtocolError) -> Self {
        match error {
            ProtocolError::Remote(code) => Self::Remote {
                code,
                description: return_code_description(code),
            },
            error => Self::Protocol(error),
        }
    }
}

fn return_code_description(code: u8) -> &'static str {
    match code {
        0x00 => "success",
        0x01 => "unsupported message",
        0x02 => "invalid message length",
        0x03 => "request denied",
        0x11 => "parameter not found",
        0x12 => "bad or out-of-range value",
        0x13 => "read-only parameter",
        0x14 => "parameter locked",
        0x15 => "bad array index",
        _ => "unknown remote error",
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::protocol::{GET_DESCRIPTOR, GET_NUMPARAMS, GET_PARAM, GET_PARAM_OIDS, HEADER_LEN};

    #[test]
    fn non_forced_connection_refusal_reports_connections_in_use() {
        assert!(matches!(
            ClientError::handshake_rejected(false, Some(0)),
            ClientError::ConnectionInUse
        ));
        assert!(matches!(
            ClientError::handshake_rejected(true, Some(0)),
            ClientError::HandshakeRejected { reason: Some(0) }
        ));
    }

    #[tokio::test]
    async fn external_objects_distinguish_layout_data_from_constraints() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            accept_handshake(&mut stream).await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_DESCRIPTOR);
            assert_eq!(request.content, vec![0, 0x12, 0x00]);
            send(
                &mut stream,
                response(
                    GET_DESCRIPTOR,
                    numeric_descriptor(0x1200, "Layout Metadata", 1, 2, 11, &[0x70, 0x01]),
                ),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_EXTERNAL_OBJECT);
            assert_eq!(request.content, vec![0, 0x70, 0x01, 0, 0]);
            send(
                &mut stream,
                response(
                    protocol::GET_EXTERNAL_OBJECT,
                    vec![0, 0x70, 0x01, 0, 0, 0, 7, 5, 0, 4, 4, 0x1F, 0x8B],
                ),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_DESCRIPTOR);
            assert_eq!(request.content, vec![0, 0x12, 0x01]);
            send(
                &mut stream,
                response(
                    GET_DESCRIPTOR,
                    numeric_descriptor(0x1201, "Operating Mode", 1, 7, 11, &[0x70, 0x02]),
                ),
            )
            .await;

            let first = [0, 1, 3, 0, 0, 2, 0];
            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_EXTERNAL_OBJECT);
            assert_eq!(request.content, vec![0, 0x70, 0x02, 0, 0]);
            send(
                &mut stream,
                response(
                    protocol::GET_EXTERNAL_OBJECT,
                    external_fragment(0x7002, 0, 7, &first),
                ),
            )
            .await;

            let remaining = [
                0, 5, b'I', b'd', b'l', b'e', 0, 0, 1, 7, b'A', b'c', b't', b'i', b'v', b'e', 0,
            ];
            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_EXTERNAL_OBJECT);
            assert_eq!(request.content, vec![0, 0x70, 0x02, 0, 7]);
            send(
                &mut stream,
                response(
                    protocol::GET_EXTERNAL_OBJECT,
                    external_fragment(0x7002, 7, 0, &remaining),
                ),
            )
            .await;
        });

        let mut client =
            OgpClient::connect_with_port("127.0.0.1", address.port(), Slot::new(1).unwrap(), true)
                .await
                .unwrap();
        let parameter = client
            .describe_parameter(&ParameterOid::Numeric(0x1200))
            .await
            .unwrap();
        assert!(matches!(
            parameter.constraint,
            Constraint::External {
                object_id: 0x7001,
                resolved,
            } if *resolved == Constraint::Unconstrained
        ));
        let operating_mode = client
            .describe_parameter(&ParameterOid::Numeric(0x1201))
            .await
            .unwrap();
        let Constraint::External { resolved, .. } = &operating_mode.constraint else {
            panic!("expected an external constraint");
        };
        let Constraint::Choice { choices } = resolved.as_ref() else {
            panic!("expected a resolved choice constraint");
        };
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[1].display_name, "Active");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn targeted_descriptors_support_choice_and_numeric_values() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            accept_handshake(&mut stream).await;

            let mode_choices = [
                2, 0, 0, 5, b'I', b'd', b'l', b'e', 0, 0, 1, 7, b'A', b'c', b't', b'i', b'v', b'e',
                0,
            ];
            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_DESCRIPTOR);
            assert_eq!(request.content, vec![0, 0x12, 0x01]);
            send(
                &mut stream,
                response(
                    GET_DESCRIPTOR,
                    numeric_descriptor(0x1201, "Primary Mode", 1, 5, 2, &mode_choices),
                ),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_PARAM);
            assert_eq!(request.content, vec![0, 0x12, 0x01]);
            send(
                &mut stream,
                response(GET_PARAM, vec![0, 0x12, 0x01, 2, 0, 0]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::SET_PARAM);
            assert_eq!(request.content, vec![0, 0x12, 0x01, 2, 0, 1]);
            send(
                &mut stream,
                response(protocol::SET_PARAM, vec![0, 0x12, 0x01, 2, 0, 1]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_DESCRIPTOR);
            assert_eq!(request.content, vec![0, 0x12, 0x02]);
            send(
                &mut stream,
                response(
                    GET_DESCRIPTOR,
                    numeric_descriptor(0x1202, "Secondary Mode", 1, 5, 2, &mode_choices),
                ),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_PARAM);
            assert_eq!(request.content, vec![0, 0x12, 0x02]);
            send(
                &mut stream,
                response(GET_PARAM, vec![0, 0x12, 0x02, 2, 0, 1]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::SET_PARAM);
            assert_eq!(request.content, vec![0, 0x12, 0x02, 2, 0, 0]);
            send(
                &mut stream,
                response(protocol::SET_PARAM, vec![0, 0x12, 0x02, 2, 0, 0]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_DESCRIPTOR);
            assert_eq!(request.content, vec![0, 0x12, 0x03]);
            send(
                &mut stream,
                response(
                    GET_DESCRIPTOR,
                    numeric_descriptor(0x1203, "Offset", 1, 9, 0, &[]),
                ),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_PARAM);
            assert_eq!(request.content, vec![0, 0x12, 0x03]);
            send(
                &mut stream,
                response(GET_PARAM, vec![0, 0x12, 0x03, 2, 1, 101]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::SET_PARAM);
            assert_eq!(request.content, vec![0, 0x12, 0x03, 2, 0, 100]);
            send(
                &mut stream,
                response(protocol::SET_PARAM, vec![0, 0x12, 0x03, 2, 0, 100]),
            )
            .await;
        });

        let mut client =
            OgpClient::connect_with_port("127.0.0.1", address.port(), Slot::new(1).unwrap(), true)
                .await
                .unwrap();

        let primary_mode = client
            .describe_parameter(&ParameterOid::Numeric(0x1201))
            .await
            .unwrap();
        assert_eq!(
            client.read_parameter(&primary_mode).await.unwrap().value,
            ParameterValue::Int16 { value: 0 }
        );
        let active = value::parse_text(&primary_mode, "Active").unwrap();
        assert_eq!(
            client
                .write_parameter(&primary_mode, active)
                .await
                .unwrap()
                .value,
            ParameterValue::Int16 { value: 1 }
        );

        let secondary_mode = client
            .describe_parameter(&ParameterOid::Numeric(0x1202))
            .await
            .unwrap();
        assert_eq!(
            client.read_parameter(&secondary_mode).await.unwrap().value,
            ParameterValue::Int16 { value: 1 }
        );
        let idle = value::parse_text(&secondary_mode, "Idle").unwrap();
        client.write_parameter(&secondary_mode, idle).await.unwrap();

        let offset = client
            .describe_parameter(&ParameterOid::Numeric(0x1203))
            .await
            .unwrap();
        assert_eq!(
            client.read_parameter(&offset).await.unwrap().value,
            ParameterValue::Int16 { value: 357 }
        );
        let new_offset = value::parse_text(&offset, "100").unwrap();
        assert_eq!(
            client
                .write_parameter(&offset, new_offset)
                .await
                .unwrap()
                .value,
            ParameterValue::Int16 { value: 100 }
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discovers_and_reads_a_numeric_parameter() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::SET_PARAM);
            send(
                &mut stream,
                Frame {
                    source: protocol::FRAME_CONTROLLER_ADDRESS,
                    destination: CLIENT_ADDRESS,
                    message_type: protocol::SET_PARAM | RESPONSE_BIT,
                    content: vec![0, 0xFF, 0x03, 2, 0, 1],
                },
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_NUMPARAMS);
            send(&mut stream, response(GET_NUMPARAMS, vec![0, 0, 1])).await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_PARAM_OIDS);
            send(
                &mut stream,
                response(GET_PARAM_OIDS, vec![0, 0, 0, 1, 0x01, 0x05]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_DESCRIPTOR);
            send(
                &mut stream,
                response(
                    GET_DESCRIPTOR,
                    vec![
                        0, 0x01, 0x04, 17, 2, 7, 32, 0, 0, 1, 8, b'P', b'r', b'o', b'd', b'u',
                        b'c', b't', 0, 0, 0,
                    ],
                ),
            )
            .await;
            send(
                &mut stream,
                response(
                    GET_DESCRIPTOR,
                    vec![
                        0, 0x01, 0x05, 17, 2, 7, 32, 0, 0, 1, 8, b'P', b'r', b'o', b'd', b'u',
                        b'c', b't', 0, 0, 0,
                    ],
                ),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_MENUSET_NAME);
            send(
                &mut stream,
                response(
                    protocol::GET_MENUSET_NAME,
                    vec![0, 0, 0, 7, b'S', b'T', b'A', b'T', b'U', b'S', 0],
                ),
            )
            .await;
            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_MENU_COUNT);
            send(
                &mut stream,
                response(protocol::GET_MENU_COUNT, vec![0, 0, 0]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_MENUSET_NAME);
            send(
                &mut stream,
                response(
                    protocol::GET_MENUSET_NAME,
                    vec![
                        0, 1, 0, 14, b'C', b'O', b'N', b'F', b'I', b'G', b'U', b'R', b'A', b'T',
                        b'I', b'O', b'N', 0,
                    ],
                ),
            )
            .await;
            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_MENU_COUNT);
            send(
                &mut stream,
                response(protocol::GET_MENU_COUNT, vec![0, 1, 1]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_MENU_NAME);
            send(
                &mut stream,
                response(
                    protocol::GET_MENU_NAME,
                    vec![0, 1, 0, 5, b'C', b'a', b'r', b'd', 0],
                ),
            )
            .await;
            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, protocol::GET_MENU_OIDS);
            send(
                &mut stream,
                response(protocol::GET_MENU_OIDS, vec![0, 1, 0, 1, 0x01, 0x05]),
            )
            .await;

            let request = receive(&mut stream).await;
            assert_eq!(request.message_type, GET_PARAM);
            send(
                &mut stream,
                response(GET_PARAM, vec![0, 0x01, 0x05, 5, b'C', b'a', b'r', b'd', 0]),
            )
            .await;
        });

        let mut client =
            OgpClient::connect_with_port("127.0.0.1", address.port(), Slot::new(1).unwrap(), true)
                .await
                .unwrap();
        let catalog = client.discover().await.unwrap();
        assert_eq!(catalog.parameters.len(), 1);
        assert_eq!(catalog.parameters[0].display_name, "Product");
        assert_eq!(catalog.menus[1].menus[0].name, "Card");
        assert_eq!(
            catalog
                .resolve_in_menu(None, "Card", &ParameterSelector::Auto("Product".into()))
                .unwrap()
                .oid,
            ParameterOid::Numeric(0x0105)
        );
        let result = client
            .read(&ParameterSelector::Oid(ParameterOid::Numeric(0x0105)))
            .await
            .unwrap();
        assert_eq!(
            result.value,
            ParameterValue::String {
                value: "Card".into()
            }
        );
        server.await.unwrap();
    }

    fn response(message_type: u8, content: Vec<u8>) -> Frame {
        Frame {
            source: 0x11,
            destination: CLIENT_ADDRESS,
            message_type: message_type | RESPONSE_BIT,
            content,
        }
    }

    fn numeric_descriptor(
        oid: u16,
        name: &str,
        access: u8,
        widget: u8,
        constraint_type: u8,
        constraint_data: &[u8],
    ) -> Vec<u8> {
        let mut content = vec![
            0,
            (oid >> 8) as u8,
            oid as u8,
            0,
            2,
            2,
            2,
            access,
            0,
            widget,
            u8::try_from(name.len() + 1).unwrap(),
        ];
        content.extend_from_slice(name.as_bytes());
        content.push(0);
        content.push(constraint_type);
        content.push(u8::try_from(constraint_data.len()).unwrap());
        content.extend_from_slice(constraint_data);
        content[3] = u8::try_from(content.len() - 4).unwrap();
        content
    }

    fn external_fragment(oid: u16, fragment: u16, next: u16, data: &[u8]) -> Vec<u8> {
        let mut content = vec![0];
        content.extend_from_slice(&oid.to_be_bytes());
        content.extend_from_slice(&fragment.to_be_bytes());
        content.extend_from_slice(&next.to_be_bytes());
        content.push(u8::try_from(data.len()).unwrap());
        content.extend_from_slice(data);
        content
    }

    async fn accept_handshake(stream: &mut TcpStream) {
        let request = receive(stream).await;
        assert_eq!(request.message_type, protocol::SET_PARAM);
        send(
            stream,
            Frame {
                source: protocol::FRAME_CONTROLLER_ADDRESS,
                destination: CLIENT_ADDRESS,
                message_type: protocol::SET_PARAM | RESPONSE_BIT,
                content: vec![0, 0xFF, 0x03, 2, 0, 1],
            },
        )
        .await;
    }

    async fn receive(stream: &mut TcpStream) -> Frame {
        let mut header = [0; HEADER_LEN];
        stream.read_exact(&mut header).await.unwrap();
        let length = u16::from_be_bytes([header[7], header[8]]) as usize;
        let mut content = vec![0; length];
        stream.read_exact(&mut content).await.unwrap();
        Frame {
            source: header[4],
            destination: header[5],
            message_type: header[6],
            content,
        }
    }

    async fn send(stream: &mut TcpStream, frame: Frame) {
        stream.write_all(&frame.encode().unwrap()).await.unwrap();
    }
}
