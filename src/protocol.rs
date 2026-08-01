use std::str;

use thiserror::Error;

use crate::model::{
    Access, AlarmDefinition, Choice, Constraint, KnownWidget, Parameter, ParameterOid,
    ParameterRole, ParameterType, ParameterValue, SetSemantics, Widget,
};

pub const SYNC: [u8; 4] = [0xBA, 0xD2, 0xAC, 0xE5];
pub const HEADER_LEN: usize = 9;
pub const MAX_CONTENT_LEN: usize = 8192;
pub const CLIENT_ADDRESS: u8 = 0x00;
pub const FRAME_CONTROLLER_ADDRESS: u8 = 0x10;
pub const GET_NUMPARAMS: u8 = 0x45;
pub const GET_PARAM_OIDS: u8 = 0x46;
pub const GET_DESCRIPTOR: u8 = 0x47;
pub const GET_PARAM: u8 = 0x49;
pub const SET_PARAM: u8 = 0x4A;
pub const GET_MENUSET_NAME: u8 = 0x50;
pub const GET_MENU_COUNT: u8 = 0x51;
pub const GET_MENU_NAME: u8 = 0x52;
pub const GET_MENU_OIDS: u8 = 0x53;
pub const GET_EXTERNAL_OBJECT: u8 = 0x59;
pub const GET_STRING_OIDS: u8 = 0x66;
pub const GET_STRING_DESCRIPTOR: u8 = 0x67;
pub const GET_STRING: u8 = 0x69;
pub const SET_STRING: u8 = 0x6A;
pub const GET_MENU_STRING_OIDS: u8 = 0x73;
pub const REPORT_PARAM: u8 = 0x10;
pub const REPORT_STRING: u8 = 0x14;
pub const RESPONSE_BIT: u8 = 0x80;
pub const CONNECTION_OID: u16 = 0xFF03;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub source: u8,
    pub destination: u8,
    pub message_type: u8,
    pub content: Vec<u8>,
}

impl Frame {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.content.len() > MAX_CONTENT_LEN || self.content.len() > u16::MAX as usize {
            return Err(ProtocolError::ContentTooLarge(self.content.len()));
        }
        let mut bytes = Vec::with_capacity(HEADER_LEN + self.content.len());
        bytes.extend_from_slice(&SYNC);
        bytes.push(self.source);
        bytes.push(self.destination);
        bytes.push(self.message_type);
        bytes.extend_from_slice(&(self.content.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&self.content);
        Ok(bytes)
    }
}

#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtocolError> {
        self.align_to_sync();
        if self.buffer.len() < HEADER_LEN {
            return Ok(None);
        }
        let content_len = u16::from_be_bytes([self.buffer[7], self.buffer[8]]) as usize;
        if content_len > MAX_CONTENT_LEN {
            self.buffer.drain(..4);
            return Err(ProtocolError::ContentTooLarge(content_len));
        }
        let frame_len = HEADER_LEN + content_len;
        if self.buffer.len() < frame_len {
            return Ok(None);
        }
        let frame = Frame {
            source: self.buffer[4],
            destination: self.buffer[5],
            message_type: self.buffer[6],
            content: self.buffer[HEADER_LEN..frame_len].to_vec(),
        };
        self.buffer.drain(..frame_len);
        Ok(Some(frame))
    }

    fn align_to_sync(&mut self) {
        if self.buffer.starts_with(&SYNC) {
            return;
        }
        if let Some(position) = self
            .buffer
            .windows(SYNC.len())
            .position(|item| item == SYNC)
        {
            self.buffer.drain(..position);
        } else if self.buffer.len() > SYNC.len() - 1 {
            let keep = SYNC.len() - 1;
            self.buffer.drain(..self.buffer.len() - keep);
        }
    }
}

fn request(destination: u8, message_type: u8, content: Vec<u8>) -> Frame {
    Frame {
        source: CLIENT_ADDRESS,
        destination,
        message_type,
        content,
    }
}

pub fn num_params_request(destination: u8, string_support: bool) -> Frame {
    let content = if string_support { vec![0, 1] } else { vec![0] };
    request(destination, GET_NUMPARAMS, content)
}

pub fn numeric_oid_page_request(destination: u8, first: u16, count: u8) -> Frame {
    let mut content = vec![0];
    content.extend_from_slice(&first.to_be_bytes());
    content.push(count);
    request(destination, GET_PARAM_OIDS, content)
}

pub fn string_oid_page_request(destination: u8, first: u32, count: u16) -> Frame {
    let mut content = vec![0];
    content.extend_from_slice(&first.to_be_bytes());
    content.extend_from_slice(&count.to_be_bytes());
    request(destination, GET_STRING_OIDS, content)
}

pub fn descriptor_request(destination: u8, oid: &ParameterOid) -> Result<Frame, ProtocolError> {
    match oid {
        ParameterOid::Numeric(oid) => {
            let mut content = vec![0];
            content.extend_from_slice(&oid.to_be_bytes());
            Ok(request(destination, GET_DESCRIPTOR, content))
        }
        ParameterOid::String(oid) => Ok(request(
            destination,
            GET_STRING_DESCRIPTOR,
            encode_string_oid(oid)?,
        )),
    }
}

pub fn get_request(destination: u8, oid: &ParameterOid) -> Result<Frame, ProtocolError> {
    match oid {
        ParameterOid::Numeric(oid) => {
            let mut content = vec![0];
            content.extend_from_slice(&oid.to_be_bytes());
            Ok(request(destination, GET_PARAM, content))
        }
        ParameterOid::String(oid) => Ok(request(destination, GET_STRING, encode_string_oid(oid)?)),
    }
}

pub fn set_request(
    destination: u8,
    oid: &ParameterOid,
    value: &[u8],
) -> Result<Frame, ProtocolError> {
    match oid {
        ParameterOid::Numeric(oid) => {
            let value_len = u8::try_from(value.len())
                .map_err(|_| ProtocolError::ParameterValueTooLarge(value.len()))?;
            let mut content = Vec::with_capacity(4 + value.len());
            content.push(0);
            content.extend_from_slice(&oid.to_be_bytes());
            content.push(value_len);
            content.extend_from_slice(value);
            Ok(request(destination, SET_PARAM, content))
        }
        ParameterOid::String(oid) => {
            let mut content = encode_string_oid(oid)?;
            content.push(u8::try_from(value.len()).unwrap_or(0));
            content.extend_from_slice(value);
            Ok(request(destination, SET_STRING, content))
        }
    }
}

pub fn external_object_request(destination: u8, oid: u16, fragment: u16) -> Frame {
    let mut content = vec![0];
    content.extend_from_slice(&oid.to_be_bytes());
    content.extend_from_slice(&fragment.to_be_bytes());
    request(destination, GET_EXTERNAL_OBJECT, content)
}

pub fn menu_group_request(destination: u8, message_type: u8, group: u8) -> Frame {
    request(destination, message_type, vec![0, group])
}

pub fn menu_request(destination: u8, message_type: u8, group: u8, menu: u8) -> Frame {
    request(destination, message_type, vec![0, group, menu])
}

pub fn menu_string_oid_page_request(
    destination: u8,
    group: u8,
    menu: u8,
    first: u16,
    count: u16,
) -> Frame {
    let mut content = vec![0, group, menu];
    content.extend_from_slice(&first.to_be_bytes());
    content.extend_from_slice(&count.to_be_bytes());
    request(destination, GET_MENU_STRING_OIDS, content)
}

pub fn handshake_request(force: bool) -> Frame {
    let mut value = Vec::with_capacity(4);
    value.extend_from_slice(&u16::from(force).to_be_bytes());
    value.extend_from_slice(&0u16.to_be_bytes());
    set_request(
        FRAME_CONTROLLER_ADDRESS,
        &ParameterOid::Numeric(CONNECTION_OID),
        &value,
    )
    .expect("four-byte handshake value is always valid")
}

fn encode_string_oid(oid: &str) -> Result<Vec<u8>, ProtocolError> {
    if oid.as_bytes().contains(&0) {
        return Err(ProtocolError::InvalidString("OID contains a NUL byte"));
    }
    let length =
        u8::try_from(oid.len() + 1).map_err(|_| ProtocolError::StringOidTooLong(oid.len()))?;
    let mut content = Vec::with_capacity(oid.len() + 3);
    content.extend_from_slice(&[0, length]);
    content.extend_from_slice(oid.as_bytes());
    content.push(0);
    Ok(content)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterCounts {
    pub numeric: u16,
    pub string: u32,
}

impl ParameterCounts {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() < 3 {
            return Err(ProtocolError::InvalidResponse(
                "parameter count is shorter than 3 bytes",
            ));
        }
        let numeric = u16::from_be_bytes([frame.content[1], frame.content[2]]);
        let string = if frame.content.len() == 3 {
            0
        } else {
            let url_len = frame.content[3] as usize;
            let offset = 4usize
                .checked_add(url_len)
                .ok_or(ProtocolError::InvalidResponse(
                    "parameter count length overflow",
                ))?;
            if frame.content.len() < offset + 4 {
                return Err(ProtocolError::InvalidResponse(
                    "extended parameter count is truncated",
                ));
            }
            u32::from_be_bytes([
                frame.content[offset],
                frame.content[offset + 1],
                frame.content[offset + 2],
                frame.content[offset + 3],
            ])
        };
        Ok(Self { numeric, string })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuGroupName {
    pub group: u8,
    pub name: String,
}

impl MenuGroupName {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() < 5 {
            return Err(ProtocolError::InvalidResponse(
                "menu group name response is too short",
            ));
        }
        let name_len = frame.content[3] as usize;
        if frame.content.len() != 4 + name_len {
            return Err(ProtocolError::InvalidResponse(
                "menu group name length does not match",
            ));
        }
        Ok(Self {
            group: frame.content[1],
            name: parse_nul_string(&frame.content[4..], "menu group name")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuCount {
    pub group: u8,
    pub count: u8,
}

impl MenuCount {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() != 3 {
            return Err(ProtocolError::InvalidResponse(
                "menu count response must contain 3 bytes",
            ));
        }
        Ok(Self {
            group: frame.content[1],
            count: frame.content[2],
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuDescription {
    pub group: u8,
    pub menu: u8,
    pub name: String,
    pub stable_id: Option<u16>,
    pub layout_url: Option<String>,
    pub string_oid_count: Option<u16>,
}

impl MenuDescription {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() < 5 {
            return Err(ProtocolError::InvalidResponse(
                "menu name response is too short",
            ));
        }
        let name_len = frame.content[3] as usize;
        let name_end = 4usize
            .checked_add(name_len)
            .ok_or(ProtocolError::InvalidResponse("menu name length overflow"))?;
        if name_end > frame.content.len() {
            return Err(ProtocolError::InvalidResponse("menu name is truncated"));
        }
        let name = parse_nul_string(&frame.content[4..name_end], "menu name")?;
        let mut offset = name_end;
        let stable_id = if frame.content.len() >= offset + 2 {
            let value = u16::from_be_bytes([frame.content[offset], frame.content[offset + 1]]);
            offset += 2;
            Some(value)
        } else {
            None
        };
        let mut layout_url = None;
        let mut string_oid_count = None;
        if offset < frame.content.len() {
            let layout_len = frame.content[offset] as usize;
            offset += 1;
            let layout_end =
                offset
                    .checked_add(layout_len)
                    .ok_or(ProtocolError::InvalidResponse(
                        "menu layout length overflow",
                    ))?;
            if layout_end > frame.content.len() {
                return Err(ProtocolError::InvalidResponse(
                    "menu layout URL is truncated",
                ));
            }
            if layout_len != 0 {
                layout_url = Some(parse_nul_string(
                    &frame.content[offset..layout_end],
                    "menu layout URL",
                )?);
            }
            offset = layout_end;
            if frame.content.len() >= offset + 2 {
                string_oid_count = Some(u16::from_be_bytes([
                    frame.content[offset],
                    frame.content[offset + 1],
                ]));
                offset += 2;
            }
        }
        if offset != frame.content.len() {
            return Err(ProtocolError::InvalidResponse(
                "menu name response has an invalid optional tail",
            ));
        }
        Ok(Self {
            group: frame.content[1],
            menu: frame.content[2],
            name,
            stable_id,
            layout_url,
            string_oid_count,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuNumericOids {
    pub group: u8,
    pub menu: u8,
    pub oids: Vec<u16>,
}

impl MenuNumericOids {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() < 4 {
            return Err(ProtocolError::InvalidResponse(
                "menu OID response is too short",
            ));
        }
        let count = frame.content[3] as usize;
        if count > 128 || frame.content.len() != 4 + count * 2 {
            return Err(ProtocolError::InvalidResponse(
                "menu OID response has an invalid length",
            ));
        }
        Ok(Self {
            group: frame.content[1],
            menu: frame.content[2],
            oids: frame.content[4..]
                .chunks_exact(2)
                .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
                .collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuStringOidPage {
    pub group: u8,
    pub menu: u8,
    pub first: u16,
    pub oids: Vec<String>,
}

impl MenuStringOidPage {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() < 7 {
            return Err(ProtocolError::InvalidResponse(
                "menu string OID response is too short",
            ));
        }
        let first = u16::from_be_bytes([frame.content[3], frame.content[4]]);
        let count = u16::from_be_bytes([frame.content[5], frame.content[6]]) as usize;
        if count > 128 {
            return Err(ProtocolError::InvalidResponse(
                "menu string OID response exceeds 128 entries",
            ));
        }
        let mut offset = 7;
        let mut oids = Vec::with_capacity(count);
        for _ in 0..count {
            let (oid, consumed) =
                parse_counted_string(&frame.content[offset..], "menu string OID")?;
            offset += consumed;
            oids.push(oid);
        }
        if offset != frame.content.len() {
            return Err(ProtocolError::InvalidResponse(
                "menu string OID response has trailing bytes",
            ));
        }
        Ok(Self {
            group: frame.content[1],
            menu: frame.content[2],
            first,
            oids,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumericOidPage {
    pub first: u16,
    pub oids: Vec<u16>,
}

impl NumericOidPage {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() < 4 {
            return Err(ProtocolError::InvalidResponse(
                "numeric OID page is shorter than 4 bytes",
            ));
        }
        let first = u16::from_be_bytes([frame.content[1], frame.content[2]]);
        let count = frame.content[3] as usize;
        if count > 128 || frame.content.len() != 4 + count * 2 {
            return Err(ProtocolError::InvalidResponse(
                "numeric OID page has an invalid length",
            ));
        }
        let oids = frame.content[4..]
            .chunks_exact(2)
            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
            .collect();
        Ok(Self { first, oids })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringOidPage {
    pub first: u32,
    pub oids: Vec<String>,
}

impl StringOidPage {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() < 7 {
            return Err(ProtocolError::InvalidResponse(
                "string OID page is shorter than 7 bytes",
            ));
        }
        let first = u32::from_be_bytes([
            frame.content[1],
            frame.content[2],
            frame.content[3],
            frame.content[4],
        ]);
        let count = u16::from_be_bytes([frame.content[5], frame.content[6]]) as usize;
        if count > 128 {
            return Err(ProtocolError::InvalidResponse(
                "string OID page exceeds 128 entries",
            ));
        }
        let mut offset = 7;
        let mut oids = Vec::with_capacity(count);
        for _ in 0..count {
            let (value, consumed) = parse_counted_string(&frame.content[offset..], "string OID")?;
            offset += consumed;
            oids.push(value);
        }
        if offset != frame.content.len() {
            return Err(ProtocolError::InvalidResponse(
                "string OID page has trailing bytes",
            ));
        }
        Ok(Self { first, oids })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDescriptor {
    pub oid: ParameterOid,
    pub version: u8,
    pub type_id: u8,
    pub size: u8,
    pub access: u8,
    pub precision: u8,
    pub widget: u8,
    pub display_name: String,
    pub constraint_type: u8,
    pub constraint_data: Vec<u8>,
}

impl RawDescriptor {
    pub fn parse(frame: &Frame, expected_oid: &ParameterOid) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        let descriptor = match expected_oid {
            ParameterOid::Numeric(_) => parse_numeric_descriptor(&frame.content)?,
            ParameterOid::String(_) => parse_string_descriptor(&frame.content)?,
        };
        if descriptor.oid != *expected_oid {
            return Err(ProtocolError::UnexpectedOid {
                expected: expected_oid.clone(),
                actual: descriptor.oid,
            });
        }
        if descriptor.version > 2 {
            return Err(ProtocolError::UnsupportedDescriptorVersion(
                descriptor.version,
            ));
        }
        Ok(descriptor)
    }

    pub fn parameter_type(&self) -> ParameterType {
        match self.type_id {
            2 => ParameterType::Int16,
            4 => ParameterType::Int32,
            6 => ParameterType::Float32,
            7 => ParameterType::String {
                max_bytes: (self.size != 0).then_some(self.size),
            },
            12 => ParameterType::Int16Array {
                length: Some((self.size / 2).into()),
            },
            14 => ParameterType::Int32Array {
                length: Some((self.size / 4).into()),
            },
            16 => ParameterType::Float32Array {
                length: Some((self.size / 4).into()),
            },
            17 => ParameterType::StringArray {
                length: None,
                max_element_bytes: (self.precision != 0).then_some(self.precision),
            },
            18 => ParameterType::Binary,
            type_id => ParameterType::Unsupported { type_id },
        }
    }

    pub fn inline_constraint(&self) -> Result<Constraint, ProtocolError> {
        parse_constraint(
            self.constraint_type,
            &self.constraint_data,
            &self.parameter_type(),
        )
    }

    pub fn into_parameter(self, constraint: Constraint) -> Result<Parameter, ProtocolError> {
        let parameter_type = self.parameter_type();
        let access = match self.access {
            0 => Access::ReadOnly,
            1 => Access::ReadWrite,
            value => return Err(ProtocolError::InvalidAccess(value)),
        };
        let widget_value = if self.version < 2 { 0 } else { self.widget };
        let known_kind = known_widget(widget_value, &parameter_type);
        let role = if matches!(
            known_kind,
            Some(KnownWidget::Title | KnownWidget::RichLabel)
        ) {
            ParameterRole::Layout
        } else {
            ParameterRole::Operational
        };
        let set_semantics = if !matches!(parameter_type, ParameterType::String { .. })
            && matches!(widget_value, 11 | 12)
        {
            SetSemantics::Trigger
        } else {
            SetSemantics::Value
        };
        Ok(Parameter {
            oid: self.oid,
            display_name: self.display_name,
            parameter_type,
            access,
            precision: (self.precision != 0).then_some(self.precision),
            widget: Widget {
                value: widget_value,
                known_kind,
            },
            role,
            set_semantics,
            constraint,
        })
    }
}

fn parse_numeric_descriptor(content: &[u8]) -> Result<RawDescriptor, ProtocolError> {
    if content.len() < 13 {
        return Err(ProtocolError::InvalidResponse(
            "numeric descriptor is too short",
        ));
    }
    let oid = ParameterOid::Numeric(u16::from_be_bytes([content[1], content[2]]));
    let descriptor_len = content[3] as usize;
    if descriptor_len != content.len() - 4 {
        return Err(ProtocolError::InvalidResponse(
            "numeric descriptor length does not match",
        ));
    }
    let name_len = content[10] as usize;
    let name_end = 11usize
        .checked_add(name_len)
        .ok_or(ProtocolError::InvalidResponse(
            "descriptor name length overflow",
        ))?;
    if name_end + 2 > content.len() {
        return Err(ProtocolError::InvalidResponse(
            "numeric descriptor name is truncated",
        ));
    }
    let display_name = parse_nul_string(&content[11..name_end], "parameter name")?;
    let constraint_type = content[name_end];
    let constraint_len = content[name_end + 1] as usize;
    if name_end + 2 + constraint_len != content.len() {
        return Err(ProtocolError::InvalidResponse(
            "numeric constraint length does not match",
        ));
    }
    Ok(RawDescriptor {
        oid,
        version: content[4],
        type_id: content[5],
        size: content[6],
        access: content[7],
        precision: content[8],
        widget: content[9],
        display_name,
        constraint_type,
        constraint_data: content[name_end + 2..].to_vec(),
    })
}

fn parse_string_descriptor(content: &[u8]) -> Result<RawDescriptor, ProtocolError> {
    if content.len() < 13 {
        return Err(ProtocolError::InvalidResponse(
            "string descriptor is too short",
        ));
    }
    let oid_len = content[1] as usize;
    let oid_end = 2usize
        .checked_add(oid_len)
        .ok_or(ProtocolError::InvalidResponse(
            "descriptor OID length overflow",
        ))?;
    if oid_end + 9 > content.len() {
        return Err(ProtocolError::InvalidResponse(
            "string descriptor OID is truncated",
        ));
    }
    let oid = ParameterOid::String(parse_nul_string(&content[2..oid_end], "string OID")?);
    let name_len = content[oid_end + 6] as usize;
    let name_start = oid_end + 7;
    let name_end = name_start
        .checked_add(name_len)
        .ok_or(ProtocolError::InvalidResponse(
            "descriptor name length overflow",
        ))?;
    if name_end + 3 > content.len() {
        return Err(ProtocolError::InvalidResponse(
            "string descriptor name is truncated",
        ));
    }
    let display_name = parse_nul_string(&content[name_start..name_end], "parameter name")?;
    let constraint_type = content[name_end];
    let constraint_len =
        u16::from_be_bytes([content[name_end + 1], content[name_end + 2]]) as usize;
    if name_end + 3 + constraint_len != content.len() {
        return Err(ProtocolError::InvalidResponse(
            "string constraint length does not match",
        ));
    }
    Ok(RawDescriptor {
        oid,
        version: content[oid_end],
        type_id: content[oid_end + 1],
        size: content[oid_end + 2],
        access: content[oid_end + 3],
        precision: content[oid_end + 4],
        widget: content[oid_end + 5],
        display_name,
        constraint_type,
        constraint_data: content[name_end + 3..].to_vec(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterResponse {
    pub oid: ParameterOid,
    pub value: Vec<u8>,
}

impl ParameterResponse {
    pub fn parse(frame: &Frame, expected_oid: &ParameterOid) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        let (oid, value) = match expected_oid {
            ParameterOid::Numeric(_) => {
                if frame.content.len() < 4 {
                    return Err(ProtocolError::InvalidResponse(
                        "numeric value response is too short",
                    ));
                }
                let oid =
                    ParameterOid::Numeric(u16::from_be_bytes([frame.content[1], frame.content[2]]));
                let length = frame.content[3] as usize;
                if frame.content.len() != 4 + length {
                    return Err(ProtocolError::InvalidResponse(
                        "numeric value length does not match",
                    ));
                }
                (oid, frame.content[4..].to_vec())
            }
            ParameterOid::String(_) => {
                if frame.content.len() < 4 {
                    return Err(ProtocolError::InvalidResponse(
                        "string-OID value response is too short",
                    ));
                }
                let oid_len = frame.content[1] as usize;
                let oid_end = 2 + oid_len;
                if oid_end + 1 > frame.content.len() {
                    return Err(ProtocolError::InvalidResponse(
                        "string-OID value response is truncated",
                    ));
                }
                let oid = ParameterOid::String(parse_nul_string(
                    &frame.content[2..oid_end],
                    "string OID",
                )?);
                let declared = frame.content[oid_end] as usize;
                let value = &frame.content[oid_end + 1..];
                if declared != 0 && declared != value.len() {
                    return Err(ProtocolError::InvalidResponse(
                        "string-OID value length does not match",
                    ));
                }
                (oid, value.to_vec())
            }
        };
        if oid != *expected_oid {
            return Err(ProtocolError::UnexpectedOid {
                expected: expected_oid.clone(),
                actual: oid,
            });
        }
        Ok(Self { oid, value })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalObjectFragment {
    pub oid: u16,
    pub fragment: u16,
    pub next: u16,
    pub data: Vec<u8>,
}

impl ExternalObjectFragment {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        ensure_success(frame)?;
        if frame.content.len() < 8 {
            return Err(ProtocolError::InvalidResponse(
                "external object fragment is too short",
            ));
        }
        let length = frame.content[7] as usize;
        if frame.content.len() != 8 + length {
            return Err(ProtocolError::InvalidResponse(
                "external object fragment length does not match",
            ));
        }
        Ok(Self {
            oid: u16::from_be_bytes([frame.content[1], frame.content[2]]),
            fragment: u16::from_be_bytes([frame.content[3], frame.content[4]]),
            next: u16::from_be_bytes([frame.content[5], frame.content[6]]),
            data: frame.content[8..].to_vec(),
        })
    }
}

pub fn external_constraint(
    bytes: &[u8],
    parameter_type: &ParameterType,
) -> Result<Constraint, ProtocolError> {
    if bytes.len() < 2 {
        return Err(ProtocolError::InvalidResponse(
            "external object type is truncated",
        ));
    }
    if u16::from_be_bytes([bytes[0], bytes[1]]) != 1 {
        return Ok(Constraint::Unconstrained);
    }
    if bytes.len() < 4 {
        return Err(ProtocolError::InvalidResponse(
            "external constraint header is truncated",
        ));
    }
    let constraint = parse_constraint(bytes[2], &bytes[4..], parameter_type)?;
    if matches!(constraint, Constraint::External { .. }) {
        return Err(ProtocolError::InvalidConstraint(
            "external constraint points to another external constraint",
        ));
    }
    Ok(constraint)
}

pub fn parse_constraint(
    type_id: u8,
    data: &[u8],
    parameter_type: &ParameterType,
) -> Result<Constraint, ProtocolError> {
    match type_id {
        0 => empty(data).map(|()| Constraint::Unconstrained),
        1 => parse_range(data, parameter_type, false),
        2 => parse_choices(data, parameter_type, false),
        3 => parse_choices(data, parameter_type, true),
        4 => parse_string_choices(data),
        5 => parse_range(data, parameter_type, true),
        10 => parse_alarms(data, parameter_type),
        11 => {
            if data.len() != 2 {
                return Err(ProtocolError::InvalidConstraint(
                    "external constraint must contain a 16-bit OID",
                ));
            }
            Ok(Constraint::External {
                object_id: u16::from_be_bytes([data[0], data[1]]),
                resolved: Box::new(Constraint::Unconstrained),
            })
        }
        type_id => Ok(Constraint::Unsupported { type_id }),
    }
}

fn parse_range(
    data: &[u8],
    parameter_type: &ParameterType,
    has_step: bool,
) -> Result<Constraint, ProtocolError> {
    let width = numeric_width(parameter_type)?;
    let base_values = if has_step { 3 } else { 2 };
    let display_values = if has_step { 5 } else { 4 };
    if data.len() != width * base_values && data.len() != width * display_values {
        return Err(ProtocolError::InvalidConstraint(
            "range has an invalid length",
        ));
    }
    let mut offset = 0;
    let mut next = || {
        let value = parse_number(parameter_type, &data[offset..offset + width]);
        offset += width;
        value
    };
    let minimum = next()?;
    let maximum = next()?;
    let has_display = data.len() == width * display_values;
    let (display_minimum, display_maximum) = if has_display {
        (Some(next()?), Some(next()?))
    } else {
        (None, None)
    };
    let step = if has_step { Some(next()?) } else { None };
    let minimum_number = minimum.number().ok_or(ProtocolError::InvalidConstraint(
        "range minimum is not numeric",
    ))?;
    let maximum_number = maximum.number().ok_or(ProtocolError::InvalidConstraint(
        "range maximum is not numeric",
    ))?;
    if !minimum_number.is_finite()
        || !maximum_number.is_finite()
        || minimum_number > maximum_number
        || step
            .as_ref()
            .and_then(ParameterValue::number)
            .is_some_and(|step| !step.is_finite() || step <= 0.0)
    {
        return Err(ProtocolError::InvalidConstraint("range values are invalid"));
    }
    Ok(Constraint::Range {
        minimum,
        maximum,
        display_minimum,
        display_maximum,
        step,
    })
}

fn parse_choices(
    data: &[u8],
    parameter_type: &ParameterType,
    extended: bool,
) -> Result<Constraint, ProtocolError> {
    let width = numeric_width(parameter_type)?;
    let (count, mut offset) = if extended {
        if data.len() < 2 {
            return Err(ProtocolError::InvalidConstraint(
                "extended choice is missing its count",
            ));
        }
        (u16::from_be_bytes([data[0], data[1]]) as usize, 2)
    } else {
        let Some(&count) = data.first() else {
            return Err(ProtocolError::InvalidConstraint(
                "choice is missing its count",
            ));
        };
        (count as usize, 1)
    };
    let mut choices = Vec::with_capacity(count);
    for _ in 0..count {
        if offset + width > data.len() {
            return Err(ProtocolError::InvalidConstraint(
                "choice value is truncated",
            ));
        }
        let value = parse_number(parameter_type, &data[offset..offset + width])?;
        offset += width;
        let (display_name, consumed) = parse_counted_string(&data[offset..], "choice name")?;
        offset += consumed;
        choices.push(Choice {
            value,
            display_name,
        });
    }
    if offset != data.len() {
        return Err(ProtocolError::InvalidConstraint(
            "choice has trailing bytes",
        ));
    }
    Ok(Constraint::Choice { choices })
}

fn parse_string_choices(data: &[u8]) -> Result<Constraint, ProtocolError> {
    if data.len() < 2 {
        return Err(ProtocolError::InvalidConstraint(
            "string choice is missing its count",
        ));
    }
    let count = u16::from_be_bytes([data[0], data[1]]) as usize;
    let mut offset = 2;
    let mut choices = Vec::with_capacity(count);
    for _ in 0..count {
        let (choice, consumed) = parse_counted_string(&data[offset..], "string choice")?;
        offset += consumed;
        choices.push(choice);
    }
    if offset != data.len() {
        return Err(ProtocolError::InvalidConstraint(
            "string choice has trailing bytes",
        ));
    }
    Ok(Constraint::StringChoice {
        choices,
        arbitrary_values_allowed: true,
    })
}

fn parse_alarms(data: &[u8], parameter_type: &ParameterType) -> Result<Constraint, ProtocolError> {
    let bit_width = match parameter_type {
        ParameterType::Int16 | ParameterType::Int16Array { .. } => 16,
        ParameterType::Int32 | ParameterType::Int32Array { .. } => 32,
        _ => {
            return Err(ProtocolError::InvalidConstraint(
                "alarm table is attached to a non-integer parameter",
            ));
        }
    };
    let Some(&count) = data.first() else {
        return Err(ProtocolError::InvalidConstraint(
            "alarm table is missing its count",
        ));
    };
    let mut offset = 1;
    let mut alarms = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if offset + 2 > data.len() {
            return Err(ProtocolError::InvalidConstraint("alarm entry is truncated"));
        }
        let bit = data[offset];
        let severity_value = data[offset + 1];
        if bit >= bit_width {
            return Err(ProtocolError::InvalidConstraint(
                "alarm bit exceeds the parameter width",
            ));
        }
        offset += 2;
        let (name, consumed) = parse_counted_string(&data[offset..], "alarm name")?;
        offset += consumed;
        alarms.push(AlarmDefinition {
            bit,
            name,
            severity_value,
        });
    }
    if offset != data.len() {
        return Err(ProtocolError::InvalidConstraint(
            "alarm table has trailing bytes",
        ));
    }
    Ok(Constraint::AlarmTable { alarms })
}

fn numeric_width(parameter_type: &ParameterType) -> Result<usize, ProtocolError> {
    match parameter_type {
        ParameterType::Int16 | ParameterType::Int16Array { .. } => Ok(2),
        ParameterType::Int32
        | ParameterType::Float32
        | ParameterType::Int32Array { .. }
        | ParameterType::Float32Array { .. } => Ok(4),
        _ => Err(ProtocolError::InvalidConstraint(
            "numeric constraint is attached to a non-numeric parameter",
        )),
    }
}

fn parse_number(
    parameter_type: &ParameterType,
    data: &[u8],
) -> Result<ParameterValue, ProtocolError> {
    match parameter_type {
        ParameterType::Int16 | ParameterType::Int16Array { .. } if data.len() == 2 => {
            Ok(ParameterValue::Int16 {
                value: i16::from_be_bytes([data[0], data[1]]),
            })
        }
        ParameterType::Int32 | ParameterType::Int32Array { .. } if data.len() == 4 => {
            Ok(ParameterValue::Int32 {
                value: i32::from_be_bytes([data[0], data[1], data[2], data[3]]),
            })
        }
        ParameterType::Float32 | ParameterType::Float32Array { .. } if data.len() == 4 => {
            Ok(ParameterValue::Float32 {
                value: f32::from_bits(u32::from_be_bytes([data[0], data[1], data[2], data[3]])),
            })
        }
        _ => Err(ProtocolError::InvalidConstraint(
            "numeric value has the wrong type or length",
        )),
    }
}

fn known_widget(value: u8, parameter_type: &ParameterType) -> Option<KnownWidget> {
    if matches!(parameter_type, ParameterType::String { .. }) {
        return match value {
            1 | 3 | 4 | 9 | 14 => Some(KnownWidget::Text),
            2 => Some(KnownWidget::Hidden),
            5..=8 | 10 => Some(KnownWidget::Title),
            11 => Some(KnownWidget::Choice),
            12 => Some(KnownWidget::Alarm),
            13 => Some(KnownWidget::RichLabel),
            _ => None,
        };
    }
    match value {
        1 | 6 | 14 => Some(KnownWidget::Text),
        2 => Some(KnownWidget::Hidden),
        3 | 4 | 17 | 19 | 24..=27 => Some(KnownWidget::Slider),
        5 | 28 => Some(KnownWidget::Spinner),
        7 | 9 | 10 | 20 | 22 | 23 | 29..=34 => Some(KnownWidget::Choice),
        8 => Some(KnownWidget::Checkbox),
        11..=13 | 18 => Some(KnownWidget::Button),
        15 | 16 => Some(KnownWidget::Title),
        _ => None,
    }
}

fn parse_counted_string(
    data: &[u8],
    field: &'static str,
) -> Result<(String, usize), ProtocolError> {
    let Some(&length) = data.first() else {
        return Err(ProtocolError::InvalidString(field));
    };
    let length = length as usize;
    if data.len() < 1 + length {
        return Err(ProtocolError::InvalidString(field));
    }
    Ok((parse_nul_string(&data[1..1 + length], field)?, 1 + length))
}

fn parse_nul_string(data: &[u8], field: &'static str) -> Result<String, ProtocolError> {
    if data.is_empty() || data.last() != Some(&0) || data[..data.len() - 1].contains(&0) {
        return Err(ProtocolError::InvalidString(field));
    }
    Ok(str::from_utf8(&data[..data.len() - 1])?.to_owned())
}

fn empty(data: &[u8]) -> Result<(), ProtocolError> {
    if data.is_empty() {
        Ok(())
    } else {
        Err(ProtocolError::InvalidConstraint("null constraint has data"))
    }
}

pub fn ensure_success(frame: &Frame) -> Result<(), ProtocolError> {
    match frame.content.first() {
        Some(0) => Ok(()),
        Some(code) => Err(ProtocolError::Remote(*code)),
        None => Err(ProtocolError::InvalidResponse("missing return code")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeResponse {
    pub allowed: bool,
    pub urm_state: Option<u16>,
    pub refusal_reason: Option<u16>,
}

impl HandshakeResponse {
    pub fn parse(frame: &Frame) -> Result<Self, ProtocolError> {
        let response = ParameterResponse::parse(frame, &ParameterOid::Numeric(CONNECTION_OID))?;
        let value = response.value;
        if value.len() < 2 || value.len() % 2 != 0 {
            return Err(ProtocolError::InvalidResponse(
                "handshake value must contain 16-bit fields",
            ));
        }
        Ok(Self {
            allowed: u16::from_be_bytes([value[0], value[1]]) != 0,
            urm_state: (value.len() >= 4).then(|| u16::from_be_bytes([value[2], value[3]])),
            refusal_reason: (value.len() >= 6).then(|| u16::from_be_bytes([value[4], value[5]])),
        })
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("OGP content is too large: {0} bytes")]
    ContentTooLarge(usize),
    #[error("parameter value is too large: {0} bytes")]
    ParameterValueTooLarge(usize),
    #[error("string OID is too long: {0} bytes")]
    StringOidTooLong(usize),
    #[error("invalid OGP response: {0}")]
    InvalidResponse(&'static str),
    #[error("invalid {0}")]
    InvalidString(&'static str),
    #[error("invalid parameter constraint: {0}")]
    InvalidConstraint(&'static str),
    #[error("unsupported descriptor version {0}")]
    UnsupportedDescriptorVersion(u8),
    #[error("invalid descriptor access value {0}")]
    InvalidAccess(u8),
    #[error("unexpected OID: expected {expected}, received {actual}")]
    UnexpectedOid {
        expected: ParameterOid,
        actual: ParameterOid,
    },
    #[error("remote returned OGP error {0:#04x}")]
    Remote(u8),
    #[error("value is not valid UTF-8: {0}")]
    Utf8(#[from] str::Utf8Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_fragmented_and_coalesced_frames() {
        let first = get_request(0x11, &ParameterOid::Numeric(0x0105))
            .unwrap()
            .encode()
            .unwrap();
        let second = num_params_request(0x11, true).encode().unwrap();
        let mut decoder = FrameDecoder::default();
        decoder.push(&first[..5]);
        assert!(decoder.next_frame().unwrap().is_none());
        decoder.push(&[first[5..].as_ref(), second.as_ref()].concat());
        assert_eq!(
            decoder.next_frame().unwrap().unwrap().message_type,
            GET_PARAM
        );
        assert_eq!(
            decoder.next_frame().unwrap().unwrap().message_type,
            GET_NUMPARAMS
        );
    }

    #[test]
    fn parses_numeric_choice_descriptor() {
        let frame = Frame {
            source: 0x11,
            destination: 0,
            message_type: GET_DESCRIPTOR | RESPONSE_BIT,
            content: vec![
                0, 0x01, 0x05, 28, 2, 2, 2, 1, 0, 7, 5, b'M', b'o', b'd', b'e', 0, 2, 14, 2, 0, 0,
                4, b'O', b'f', b'f', 0, 0, 1, 3, b'O', b'n', 0,
            ],
        };
        let descriptor = RawDescriptor::parse(&frame, &ParameterOid::Numeric(0x0105)).unwrap();
        let constraint = descriptor.inline_constraint().unwrap();
        assert!(matches!(constraint, Constraint::Choice { choices } if choices.len() == 2));
    }

    #[test]
    fn parses_string_oid_page() {
        let frame = Frame {
            source: 0x11,
            destination: 0,
            message_type: GET_STRING_OIDS | RESPONSE_BIT,
            content: vec![
                0, 0, 0, 0, 0, 0, 2, 5, b'g', b'a', b'i', b'n', 0, 5, b'm', b'o', b'd', b'e', 0,
            ],
        };
        assert_eq!(StringOidPage::parse(&frame).unwrap().oids, ["gain", "mode"]);
    }

    #[test]
    fn parses_extended_counts_and_string_descriptor() {
        let counts = Frame {
            source: 0x11,
            destination: 0,
            message_type: GET_NUMPARAMS | RESPONSE_BIT,
            content: vec![0, 0, 1, 0, 0, 0, 0, 1],
        };
        assert_eq!(
            ParameterCounts::parse(&counts).unwrap(),
            ParameterCounts {
                numeric: 1,
                string: 1
            }
        );

        let descriptor = Frame {
            source: 0x11,
            destination: 0,
            message_type: GET_STRING_DESCRIPTOR | RESPONSE_BIT,
            content: vec![
                0, 11, b'm', b'o', b'd', b'e', b'.', b'v', b'a', b'l', b'u', b'e', 0, 2, 2, 2, 1,
                0, 7, 5, b'M', b'o', b'd', b'e', 0, 0, 0, 0,
            ],
        };
        let oid = ParameterOid::String("mode.value".into());
        let descriptor = RawDescriptor::parse(&descriptor, &oid).unwrap();
        assert_eq!(descriptor.display_name, "Mode");
        assert_eq!(
            descriptor.inline_constraint().unwrap(),
            Constraint::Unconstrained
        );
    }

    #[test]
    fn parses_external_extended_choices() {
        let bytes = [
            0, 1, 3, 0, 0, 2, 0, 0, 4, b'O', b'f', b'f', 0, 0, 1, 3, b'O', b'n', 0,
        ];
        let constraint = external_constraint(&bytes, &ParameterType::Int16).unwrap();
        assert!(
            matches!(constraint, Constraint::Choice { choices } if choices.len() == 2 && choices[1].display_name == "On")
        );
    }

    #[test]
    fn treats_non_constraint_external_objects_as_unconstrained() {
        let constraint =
            external_constraint(&[0, 4, 4, 0x1F, 0x8B], &ParameterType::Int16).unwrap();
        assert_eq!(constraint, Constraint::Unconstrained);
    }

    #[test]
    fn rejects_truncated_external_object_headers() {
        assert!(matches!(
            external_constraint(&[0], &ParameterType::Int16),
            Err(ProtocolError::InvalidResponse(
                "external object type is truncated"
            ))
        ));
        assert!(matches!(
            external_constraint(&[0, 1, 3], &ParameterType::Int16),
            Err(ProtocolError::InvalidResponse(
                "external constraint header is truncated"
            ))
        ));
    }

    #[test]
    fn parses_constraints_for_array_elements() {
        let data = [1, 0, 1, 4, b'O', b'n', b'e', 0];
        let constraint =
            parse_constraint(2, &data, &ParameterType::Int16Array { length: Some(4) }).unwrap();
        assert!(
            matches!(constraint, Constraint::Choice { choices } if choices[0].value == ParameterValue::Int16 { value: 1 })
        );
    }

    #[test]
    fn rejects_alarm_bits_outside_the_parameter_width() {
        let data = [1, 16, 2, 6, b'A', b'l', b'a', b'r', b'm', 0];
        assert!(parse_constraint(10, &data, &ParameterType::Int16).is_err());
    }

    #[test]
    fn long_string_oid_sets_use_zero_length_marker() {
        let oid = ParameterOid::String("long.value".into());
        let value = vec![b'a'; 256];
        let frame = set_request(0x11, &oid, &value).unwrap();
        let oid_length = frame.content[1] as usize;
        assert_eq!(frame.content[2 + oid_length], 0);
        assert_eq!(&frame.content[3 + oid_length..], value);
    }

    #[test]
    fn parses_menu_name_with_string_oid_metadata() {
        let frame = Frame {
            source: 0x11,
            destination: 0,
            message_type: GET_MENU_NAME | RESPONSE_BIT,
            content: vec![
                0, 1, 2, 9, b'S', b'D', b'I', b' ', b'I', b'n', b' ', b'1', 0, 0x12, 0x34, 0, 0, 2,
            ],
        };
        let menu = MenuDescription::parse(&frame).unwrap();
        assert_eq!(menu.name, "SDI In 1");
        assert_eq!(menu.stable_id, Some(0x1234));
        assert_eq!(menu.string_oid_count, Some(2));
    }

    #[test]
    fn parses_numeric_and_string_menu_members() {
        let numeric = Frame {
            source: 0x11,
            destination: 0,
            message_type: GET_MENU_OIDS | RESPONSE_BIT,
            content: vec![0, 1, 0, 2, 0x05, 0x00, 0x05, 0x01],
        };
        assert_eq!(
            MenuNumericOids::parse(&numeric).unwrap().oids,
            [0x0500, 0x0501]
        );

        let strings = Frame {
            source: 0x11,
            destination: 0,
            message_type: GET_MENU_STRING_OIDS | RESPONSE_BIT,
            content: vec![
                0, 1, 0, 0, 0, 0, 2, 5, b'g', b'a', b'i', b'n', 0, 5, b'm', b'o', b'd', b'e', 0,
            ],
        };
        assert_eq!(
            MenuStringOidPage::parse(&strings).unwrap().oids,
            ["gain", "mode"]
        );
    }

    #[test]
    fn forced_handshake_has_canonical_wire_bytes() {
        assert_eq!(
            handshake_request(true).encode().unwrap(),
            [
                0xBA, 0xD2, 0xAC, 0xE5, 0x00, 0x10, 0x4A, 0x00, 0x08, 0x00, 0xFF, 0x03, 0x04, 0x00,
                0x01, 0x00, 0x00,
            ]
        );
    }
}
