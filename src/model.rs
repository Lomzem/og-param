use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 5;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceCatalog {
    pub parameters: Vec<Parameter>,
    pub menus: Vec<MenuGroup>,
    pub menu_discovery_error: Option<String>,
    pub incomplete_menu_groups: Vec<u8>,
}

impl DeviceCatalog {
    pub fn new(parameters: Vec<Parameter>) -> Result<Self, CatalogError> {
        let mut oids = HashSet::new();
        for parameter in &parameters {
            if !oids.insert(parameter.oid.clone()) {
                return Err(CatalogError::DuplicateOid(parameter.oid.clone()));
            }
        }
        Ok(Self {
            parameters,
            menus: Vec::new(),
            menu_discovery_error: Some("menu discovery was not performed".into()),
            incomplete_menu_groups: vec![0, 1],
        })
    }

    pub fn set_menus(
        &mut self,
        menus: Vec<MenuGroup>,
        error: Option<String>,
        incomplete_groups: Vec<u8>,
    ) {
        self.menus = menus;
        self.menu_discovery_error = error;
        self.incomplete_menu_groups = incomplete_groups;
    }

    pub fn resolve(&self, selector: &ParameterSelector) -> Result<&Parameter, ResolveError> {
        match selector {
            ParameterSelector::Oid(oid) => self
                .parameters
                .iter()
                .find(|parameter| parameter.oid == *oid)
                .ok_or_else(|| ResolveError::NotFound(selector.to_string())),
            ParameterSelector::DisplayName(name) => self.resolve_display_name(name),
            ParameterSelector::Auto(value) => self.resolve_auto(value),
        }
    }

    fn resolve_display_name(&self, name: &str) -> Result<&Parameter, ResolveError> {
        let matches: Vec<_> = self
            .parameters
            .iter()
            .filter(|parameter| label_eq(&parameter.display_name, name))
            .collect();
        match matches.as_slice() {
            [] => Err(ResolveError::NotFound(name.to_owned())),
            [parameter] => Ok(parameter),
            _ => Err(ResolveError::AmbiguousDisplayName {
                name: name.to_owned(),
                candidates: matches.iter().map(|item| self.candidate(item)).collect(),
            }),
        }
    }

    fn resolve_auto(&self, value: &str) -> Result<&Parameter, ResolveError> {
        let mut matches = Vec::new();
        if let Ok(oid) = ParameterOid::from_str(value)
            && let Some(parameter) = self.parameters.iter().find(|item| item.oid == oid)
        {
            matches.push(parameter);
        }
        for parameter in &self.parameters {
            if label_eq(&parameter.display_name, value)
                && !matches.iter().any(|item| item.oid == parameter.oid)
            {
                matches.push(parameter);
            }
        }
        match matches.as_slice() {
            [] => Err(ResolveError::NotFound(value.to_owned())),
            [parameter] => Ok(parameter),
            _ => Err(ResolveError::AmbiguousDisplayName {
                name: value.to_owned(),
                candidates: matches.iter().map(|item| self.candidate(item)).collect(),
            }),
        }
    }

    pub fn resolve_in_menu(
        &self,
        group_name: Option<&str>,
        menu_name: &str,
        selector: &ParameterSelector,
    ) -> Result<&Parameter, ResolveError> {
        self.ensure_menu_scope_complete(group_name)?;
        self.ensure_group_name_unique(group_name)?;

        let matching_menus: Vec<_> = self
            .menus
            .iter()
            .filter(|group| group_name.is_none_or(|name| label_eq(&group.name, name)))
            .flat_map(|group| {
                group
                    .menus
                    .iter()
                    .filter(|menu| label_eq(&menu.name, menu_name))
                    .map(move |menu| (group, menu))
            })
            .collect();

        match matching_menus.as_slice() {
            [] => Err(self.missing_menu_error(group_name, menu_name)),
            [(group, menu)] => self.resolve_menu_parameter(group, menu, selector),
            _ => Err(ResolveError::AmbiguousMenu {
                menu: menu_name.to_owned(),
                candidates: matching_menus
                    .iter()
                    .map(|(group, menu)| MenuIdentity {
                        group: group.name.clone(),
                        menu: menu.name.clone(),
                        group_index: group.index,
                        menu_index: menu.index,
                    })
                    .collect(),
            }),
        }
    }

    pub fn resolve_menu(
        &self,
        group_name: Option<&str>,
        menu_name: &str,
    ) -> Result<(&MenuGroup, &Menu), ResolveError> {
        self.ensure_menu_scope_complete(group_name)?;
        self.ensure_group_name_unique(group_name)?;
        let matches: Vec<_> = self
            .menus
            .iter()
            .filter(|group| group_name.is_none_or(|name| label_eq(&group.name, name)))
            .flat_map(|group| {
                group
                    .menus
                    .iter()
                    .filter(|menu| label_eq(&menu.name, menu_name))
                    .map(move |menu| (group, menu))
            })
            .collect();
        match matches.as_slice() {
            [] => Err(self.missing_menu_error(group_name, menu_name)),
            [item] => Ok(*item),
            _ => Err(ResolveError::AmbiguousMenu {
                menu: menu_name.to_owned(),
                candidates: matches
                    .iter()
                    .map(|(group, menu)| MenuIdentity {
                        group: group.name.clone(),
                        menu: menu.name.clone(),
                        group_index: group.index,
                        menu_index: menu.index,
                    })
                    .collect(),
            }),
        }
    }

    pub fn parameters_in_menu(&self, menu: &Menu) -> Vec<&Parameter> {
        let mut seen = HashSet::new();
        menu.members
            .iter()
            .filter(|oid| seen.insert((*oid).clone()))
            .filter_map(|oid| {
                self.parameters
                    .iter()
                    .find(|parameter| parameter.oid == *oid)
            })
            .collect()
    }

    pub fn menu_paths(&self, parameter: &Parameter) -> Vec<MenuPath> {
        let mut paths = Vec::new();
        for group in &self.menus {
            for menu in &group.menus {
                if menu.members.contains(&parameter.oid) {
                    let path = MenuPath {
                        group: group.name.clone(),
                        menu: menu.name.clone(),
                        group_required: self.menu_name_is_ambiguous(&menu.name),
                    };
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
            }
        }
        paths
    }

    fn missing_menu_error(&self, group_name: Option<&str>, menu_name: &str) -> ResolveError {
        if let Some(error) = &self.menu_discovery_error {
            ResolveError::MenusUnavailable(error.clone())
        } else {
            ResolveError::MenuNotFound {
                group: group_name.map(str::to_owned),
                menu: menu_name.to_owned(),
            }
        }
    }

    fn ensure_menu_scope_complete(&self, group_name: Option<&str>) -> Result<(), ResolveError> {
        if self.incomplete_menu_groups.is_empty() {
            return Ok(());
        }
        let has_unknown_group_name = self
            .incomplete_menu_groups
            .iter()
            .any(|index| !self.menus.iter().any(|group| group.index == *index));
        if has_unknown_group_name {
            return Err(ResolveError::MenusUnavailable(
                self.menu_discovery_error
                    .clone()
                    .unwrap_or_else(|| "a menu group name is unavailable".into()),
            ));
        }
        if let Some(group_name) = group_name {
            let matching_groups: Vec<_> = self
                .menus
                .iter()
                .filter(|group| label_eq(&group.name, group_name))
                .collect();
            if matching_groups.len() == 1
                && !self
                    .incomplete_menu_groups
                    .contains(&matching_groups[0].index)
            {
                return Ok(());
            }
        }
        Err(ResolveError::MenusUnavailable(
            self.menu_discovery_error
                .clone()
                .unwrap_or_else(|| "menu discovery is incomplete".into()),
        ))
    }

    fn resolve_menu_parameter(
        &self,
        group: &MenuGroup,
        menu: &Menu,
        selector: &ParameterSelector,
    ) -> Result<&Parameter, ResolveError> {
        let members = self.parameters_in_menu(menu);
        let by_oid = |oid: &ParameterOid| members.iter().copied().find(|item| item.oid == *oid);
        match selector {
            ParameterSelector::Oid(oid) => by_oid(oid),
            ParameterSelector::DisplayName(name) => {
                return resolve_menu_display_name(
                    group,
                    menu,
                    &members,
                    name,
                    self.menu_name_is_ambiguous(&menu.name),
                );
            }
            ParameterSelector::Auto(value) => {
                return resolve_menu_auto(
                    group,
                    menu,
                    &members,
                    value,
                    self.menu_name_is_ambiguous(&menu.name),
                );
            }
        }
        .ok_or_else(|| ResolveError::ParameterNotFoundInMenu {
            menu: menu.name.clone(),
            selector: selector.to_string(),
        })
    }

    fn candidate(&self, parameter: &Parameter) -> ParameterCandidate {
        ParameterCandidate {
            parameter: parameter.identity(),
            menus: self.menu_paths(parameter),
        }
    }

    fn ensure_group_name_unique(&self, group_name: Option<&str>) -> Result<(), ResolveError> {
        let Some(group_name) = group_name else {
            return Ok(());
        };
        let candidates: Vec<_> = self
            .menus
            .iter()
            .filter(|group| label_eq(&group.name, group_name))
            .map(|group| MenuGroupIdentity {
                group: group.name.clone(),
                group_index: group.index,
            })
            .collect();
        if candidates.len() > 1 {
            Err(ResolveError::AmbiguousGroup {
                group: group_name.to_owned(),
                candidates,
            })
        } else {
            Ok(())
        }
    }

    fn menu_name_is_ambiguous(&self, menu_name: &str) -> bool {
        self.menus
            .iter()
            .flat_map(|group| &group.menus)
            .filter(|menu| label_eq(&menu.name, menu_name))
            .count()
            > 1
    }
}

fn resolve_menu_auto<'a>(
    group: &MenuGroup,
    menu: &Menu,
    members: &[&'a Parameter],
    value: &str,
    group_required: bool,
) -> Result<&'a Parameter, ResolveError> {
    let mut matches = Vec::new();
    if let Ok(oid) = ParameterOid::from_str(value)
        && let Some(parameter) = members.iter().copied().find(|item| item.oid == oid)
    {
        matches.push(parameter);
    }
    for parameter in members {
        if label_eq(&parameter.display_name, value)
            && !matches.iter().any(|item| item.oid == parameter.oid)
        {
            matches.push(*parameter);
        }
    }
    match matches.as_slice() {
        [] => Err(ResolveError::ParameterNotFoundInMenu {
            menu: menu.name.clone(),
            selector: value.to_owned(),
        }),
        [parameter] => Ok(parameter),
        _ => Err(ResolveError::AmbiguousDisplayName {
            name: value.to_owned(),
            candidates: matches
                .iter()
                .map(|item| ParameterCandidate {
                    parameter: item.identity(),
                    menus: vec![MenuPath {
                        group: group.name.clone(),
                        menu: menu.name.clone(),
                        group_required,
                    }],
                })
                .collect(),
        }),
    }
}

fn resolve_menu_display_name<'a>(
    group: &MenuGroup,
    menu: &Menu,
    members: &[&'a Parameter],
    name: &str,
    group_required: bool,
) -> Result<&'a Parameter, ResolveError> {
    let matches: Vec<_> = members
        .iter()
        .copied()
        .filter(|parameter| label_eq(&parameter.display_name, name))
        .collect();
    match matches.as_slice() {
        [] => Err(ResolveError::ParameterNotFoundInMenu {
            menu: menu.name.clone(),
            selector: name.to_owned(),
        }),
        [parameter] => Ok(parameter),
        _ => Err(ResolveError::AmbiguousDisplayName {
            name: name.to_owned(),
            candidates: matches
                .iter()
                .map(|item| ParameterCandidate {
                    parameter: item.identity(),
                    menus: vec![MenuPath {
                        group: group.name.clone(),
                        menu: menu.name.clone(),
                        group_required,
                    }],
                })
                .collect(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MenuGroup {
    pub index: u8,
    pub name: String,
    pub menus: Vec<Menu>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Menu {
    pub index: u8,
    pub name: String,
    pub stable_id: u16,
    pub layout_url: Option<String>,
    pub members: Vec<ParameterOid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MenuIdentity {
    pub group: String,
    pub menu: String,
    pub group_index: u8,
    pub menu_index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MenuGroupIdentity {
    pub group: String,
    pub group_index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MenuPath {
    pub group: String,
    pub menu: String,
    #[serde(skip)]
    pub group_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParameterCandidate {
    pub parameter: ParameterIdentity,
    pub menus: Vec<MenuPath>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Parameter {
    pub oid: ParameterOid,
    pub display_name: String,
    pub parameter_type: ParameterType,
    pub access: Access,
    pub precision: Option<u8>,
    pub widget: Widget,
    pub role: ParameterRole,
    pub set_semantics: SetSemantics,
    pub constraint: Constraint,
}

impl Parameter {
    pub fn identity(&self) -> ParameterIdentity {
        ParameterIdentity {
            oid: self.oid.clone(),
            display_name: self.display_name.clone(),
        }
    }

    pub fn is_runtime_supported(&self) -> bool {
        matches!(
            self.parameter_type,
            ParameterType::Int16
                | ParameterType::Int32
                | ParameterType::Float32
                | ParameterType::String { .. }
        )
    }

    pub fn interpret(&self, value: &ParameterValue) -> Option<DisplayValue> {
        match self.resolved_constraint() {
            Constraint::Choice { choices } => {
                let mut labels = Vec::new();
                for label in choices
                    .iter()
                    .filter(|choice| choice.value == *value)
                    .map(|choice| &choice.display_name)
                {
                    if !labels.contains(label) {
                        labels.push(label.clone());
                    }
                }
                match labels.as_slice() {
                    [] => None,
                    [label] => Some(DisplayValue::Label {
                        value: label.clone(),
                    }),
                    _ => Some(DisplayValue::Aliases { values: labels }),
                }
            }
            Constraint::StringChoice { choices, .. } => match value {
                ParameterValue::String { value } if choices.contains(value) => {
                    Some(DisplayValue::Label {
                        value: value.clone(),
                    })
                }
                _ => None,
            },
            Constraint::AlarmTable { alarms } => {
                value.integer_bits().map(|bits| DisplayValue::Alarms {
                    value: decode_alarms(bits, alarms),
                })
            }
            Constraint::Range {
                minimum,
                maximum,
                display_minimum: Some(display_minimum),
                display_maximum: Some(display_maximum),
                ..
            } => map_display_number(value, minimum, maximum, display_minimum, display_maximum).map(
                |number| DisplayValue::Numeric {
                    value: number,
                    formatted: format!("{:.*}", self.precision.unwrap_or(0) as usize, number),
                },
            ),
            _ => None,
        }
    }

    pub fn resolved_constraint(&self) -> &Constraint {
        match &self.constraint {
            Constraint::External { resolved, .. } => resolved,
            constraint => constraint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParameterIdentity {
    pub oid: ParameterOid,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ParameterOid {
    Numeric(u16),
    String(String),
}

impl ParameterOid {
    pub fn as_numeric(&self) -> Option<u16> {
        match self {
            Self::Numeric(value) => Some(*value),
            Self::String(_) => None,
        }
    }
}

impl fmt::Display for ParameterOid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Numeric(value) => write!(formatter, "0x{value:04X}"),
            Self::String(value) => formatter.write_str(value),
        }
    }
}

impl FromStr for ParameterOid {
    type Err = ParseOidError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if let Some(hex) = input
            .strip_prefix("0x")
            .or_else(|| input.strip_prefix("0X"))
        {
            return u16::from_str_radix(hex, 16)
                .map(Self::Numeric)
                .map_err(|_| ParseOidError(input.to_owned()));
        }
        if let Ok(value) = input.parse::<u16>() {
            return Ok(Self::Numeric(value));
        }
        validate_string_oid(input)?;
        Ok(Self::String(input.to_owned()))
    }
}

impl Serialize for ParameterOid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

fn validate_string_oid(value: &str) -> Result<(), ParseOidError> {
    let valid = !value.is_empty()
        && value.len() < u8::MAX as usize
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && !value.rsplit_once('.').is_some_and(|(_, suffix)| {
            !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(ParseOidError(value.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterSelector {
    Oid(ParameterOid),
    DisplayName(String),
    Auto(String),
}

impl fmt::Display for ParameterSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oid(oid) => write!(formatter, "oid:{oid}"),
            Self::DisplayName(value) => write!(formatter, "name:{value}"),
            Self::Auto(value) => formatter.write_str(value),
        }
    }
}

impl FromStr for ParameterSelector {
    type Err = ParseSelectorError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.starts_with("symbol:") {
            return Err(ParseSelectorError(
                "symbol selectors are not supported; use an OID or display name".into(),
            ));
        }
        if let Some(value) = input.strip_prefix("oid:") {
            return ParameterOid::from_str(value)
                .map(Self::Oid)
                .map_err(|_| ParseSelectorError(format!("invalid parameter selector: {input}")));
        }
        if let Some(value) = input.strip_prefix("name:") {
            return nonempty(value, input).map(Self::DisplayName);
        }
        nonempty(input, input).map(Self::Auto)
    }
}

fn nonempty(value: &str, original: &str) -> Result<String, ParseSelectorError> {
    if value.is_empty() {
        Err(ParseSelectorError(format!(
            "invalid parameter selector: {original}"
        )))
    } else {
        Ok(value.to_owned())
    }
}

pub fn label_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterType {
    Int16,
    Int32,
    Float32,
    String {
        max_bytes: Option<u8>,
    },
    Int16Array {
        length: Option<u16>,
    },
    Int32Array {
        length: Option<u16>,
    },
    Float32Array {
        length: Option<u16>,
    },
    StringArray {
        length: Option<u16>,
        max_element_bytes: Option<u8>,
    },
    Binary,
    Unsupported {
        type_id: u8,
    },
}

impl ParameterType {
    pub fn accepts(&self, value: &ParameterValue) -> bool {
        matches!(
            (self, value),
            (Self::Int16, ParameterValue::Int16 { .. })
                | (Self::Int32, ParameterValue::Int32 { .. })
                | (Self::Float32, ParameterValue::Float32 { .. })
                | (Self::String { .. }, ParameterValue::String { .. })
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Widget {
    pub value: u8,
    pub known_kind: Option<KnownWidget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownWidget {
    Button,
    Choice,
    Text,
    Checkbox,
    Spinner,
    Slider,
    Alarm,
    Hidden,
    Title,
    RichLabel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterRole {
    Operational,
    Layout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetSemantics {
    Value,
    Trigger,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParameterValue {
    Int16 { value: i16 },
    Int32 { value: i32 },
    Float32 { value: f32 },
    String { value: String },
}

impl ParameterValue {
    pub fn number(&self) -> Option<f64> {
        match self {
            Self::Int16 { value } => Some((*value).into()),
            Self::Int32 { value } => Some((*value).into()),
            Self::Float32 { value } => Some((*value).into()),
            Self::String { .. } => None,
        }
    }

    pub fn integer_bits(&self) -> Option<u32> {
        match self {
            Self::Int16 { value } => Some(u16::from_be_bytes(value.to_be_bytes()).into()),
            Self::Int32 { value } => Some(u32::from_be_bytes(value.to_be_bytes())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DisplayValue {
    Label { value: String },
    Aliases { values: Vec<String> },
    Alarms { value: AlarmDisplay },
    Numeric { value: f64, formatted: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Constraint {
    Unconstrained,
    Choice {
        choices: Vec<Choice>,
    },
    StringChoice {
        choices: Vec<String>,
        arbitrary_values_allowed: bool,
    },
    Range {
        minimum: ParameterValue,
        maximum: ParameterValue,
        display_minimum: Option<ParameterValue>,
        display_maximum: Option<ParameterValue>,
        step: Option<ParameterValue>,
    },
    AlarmTable {
        alarms: Vec<AlarmDefinition>,
    },
    External {
        object_id: u16,
        resolved: Box<Constraint>,
    },
    Unsupported {
        type_id: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Choice {
    pub value: ParameterValue,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlarmDefinition {
    pub bit: u8,
    pub name: String,
    pub severity_value: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlarmDisplay {
    pub active: Vec<ActiveAlarm>,
    pub unknown_mask: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveAlarm {
    pub bit: u8,
    pub mask: u32,
    pub name: String,
    pub severity_value: u8,
}

fn decode_alarms(bits: u32, alarms: &[AlarmDefinition]) -> AlarmDisplay {
    let mut known_mask = 0;
    let mut active = Vec::new();
    for alarm in alarms {
        let mask = 1u32 << alarm.bit;
        known_mask |= mask;
        if bits & mask != 0 {
            active.push(ActiveAlarm {
                bit: alarm.bit,
                mask,
                name: alarm.name.clone(),
                severity_value: alarm.severity_value,
            });
        }
    }
    AlarmDisplay {
        active,
        unknown_mask: bits & !known_mask,
    }
}

fn map_display_number(
    value: &ParameterValue,
    minimum: &ParameterValue,
    maximum: &ParameterValue,
    display_minimum: &ParameterValue,
    display_maximum: &ParameterValue,
) -> Option<f64> {
    let value = value.number()?;
    let minimum = minimum.number()?;
    let maximum = maximum.number()?;
    let display_minimum = display_minimum.number()?;
    let display_maximum = display_maximum.number()?;
    if maximum == minimum {
        return None;
    }
    Some(
        display_minimum
            + (value - minimum) * (display_maximum - display_minimum) / (maximum - minimum),
    )
}

#[derive(Debug, Error)]
#[error("invalid OID: {0}")]
pub struct ParseOidError(String);

#[derive(Debug, Error)]
#[error("{0}")]
pub struct ParseSelectorError(String);

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("parameter not found: {0}")]
    NotFound(String),
    #[error("display name {name:?} is ambiguous")]
    AmbiguousDisplayName {
        name: String,
        candidates: Vec<ParameterCandidate>,
    },
    #[error("menu metadata is unavailable: {0}")]
    MenusUnavailable(String),
    #[error("menu {menu:?} was not found{group_suffix}", group_suffix = group.as_ref().map(|value| format!(" in group {value:?}")).unwrap_or_default())]
    MenuNotFound { group: Option<String>, menu: String },
    #[error("menu name {menu:?} is ambiguous")]
    AmbiguousMenu {
        menu: String,
        candidates: Vec<MenuIdentity>,
    },
    #[error("menu group name {group:?} is ambiguous")]
    AmbiguousGroup {
        group: String,
        candidates: Vec<MenuGroupIdentity>,
    },
    #[error("parameter {selector:?} was not found in menu {menu:?}")]
    ParameterNotFoundInMenu { menu: String, selector: String },
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("device returned duplicate OID {0}")]
    DuplicateOid(ParameterOid),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oids_round_trip_as_canonical_strings() {
        assert_eq!(ParameterOid::from_str("261").unwrap().to_string(), "0x0105");
        assert_eq!(
            ParameterOid::from_str("mle.keyer.state")
                .unwrap()
                .to_string(),
            "mle.keyer.state"
        );
    }

    #[test]
    fn duplicate_display_names_require_an_oid() {
        let catalog = DeviceCatalog::new(vec![
            parameter(ParameterOid::Numeric(1), "Duplicate"),
            parameter(ParameterOid::String("duplicate.value".into()), "duplicate"),
        ])
        .unwrap();
        let error = catalog
            .resolve(&ParameterSelector::DisplayName("DUPLICATE".into()))
            .unwrap_err();
        assert!(
            matches!(error, ResolveError::AmbiguousDisplayName { candidates, .. } if candidates.len() == 2)
        );
    }

    #[test]
    fn display_menu_and_group_labels_ignore_ascii_case() {
        let mut catalog =
            DeviceCatalog::new(vec![parameter(ParameterOid::Numeric(0x0500), "Colorspace")])
                .unwrap();
        catalog.set_menus(
            vec![MenuGroup {
                index: 1,
                name: "CONFIGURATION".into(),
                menus: vec![menu(0, "SDI In 1", 0x0500)],
            }],
            None,
            Vec::new(),
        );
        assert_eq!(
            catalog
                .resolve_in_menu(
                    Some("configuration"),
                    "sdi in 1",
                    &ParameterSelector::Auto("COLORSPACE".into()),
                )
                .unwrap()
                .oid,
            ParameterOid::Numeric(0x0500)
        );
    }

    #[test]
    fn string_oids_remain_case_sensitive() {
        let catalog = DeviceCatalog::new(vec![parameter(
            ParameterOid::String("Mode.Value".into()),
            "Mode",
        )])
        .unwrap();
        assert!(
            catalog
                .resolve(&ParameterSelector::Oid(ParameterOid::String(
                    "mode.value".into()
                )))
                .is_err()
        );
    }

    #[test]
    fn auto_selector_reports_oid_and_display_name_collisions() {
        let catalog = DeviceCatalog::new(vec![
            parameter(ParameterOid::String("Mode".into()), "Other"),
            parameter(ParameterOid::Numeric(2), "mode"),
        ])
        .unwrap();
        assert!(matches!(
            catalog.resolve(&ParameterSelector::Auto("Mode".into())),
            Err(ResolveError::AmbiguousDisplayName { candidates, .. }) if candidates.len() == 2
        ));
        assert_eq!(
            catalog
                .resolve(&ParameterSelector::Oid(ParameterOid::String("Mode".into())))
                .unwrap()
                .display_name,
            "Other"
        );
        assert_eq!(
            catalog
                .resolve(&ParameterSelector::DisplayName("Mode".into()))
                .unwrap()
                .oid,
            ParameterOid::Numeric(2)
        );
    }

    #[test]
    fn group_names_differing_only_by_case_are_ambiguous() {
        let mut catalog =
            DeviceCatalog::new(vec![parameter(ParameterOid::Numeric(1), "Parameter")]).unwrap();
        catalog.set_menus(
            vec![
                MenuGroup {
                    index: 0,
                    name: "STATUS".into(),
                    menus: vec![menu(0, "Card", 1)],
                },
                MenuGroup {
                    index: 1,
                    name: "status".into(),
                    menus: Vec::new(),
                },
            ],
            None,
            Vec::new(),
        );
        assert!(matches!(
            catalog.resolve_menu(Some("Status"), "Card"),
            Err(ResolveError::AmbiguousGroup { .. })
        ));
    }

    #[test]
    fn menu_context_disambiguates_display_names() {
        let mut catalog = DeviceCatalog::new(vec![
            parameter(ParameterOid::Numeric(0x0500), "Colorspace"),
            parameter(ParameterOid::Numeric(0x0600), "Colorspace"),
        ])
        .unwrap();
        catalog.set_menus(
            vec![MenuGroup {
                index: 1,
                name: "CONFIGURATION".into(),
                menus: vec![
                    Menu {
                        index: 0,
                        name: "SDI In 1".into(),
                        stable_id: 0x0100,
                        layout_url: None,
                        members: vec![ParameterOid::Numeric(0x0500)],
                    },
                    Menu {
                        index: 1,
                        name: "SDI In 2".into(),
                        stable_id: 0x0101,
                        layout_url: None,
                        members: vec![ParameterOid::Numeric(0x0600)],
                    },
                ],
            }],
            None,
            Vec::new(),
        );
        let parameter = catalog
            .resolve_in_menu(
                None,
                "SDI In 1",
                &ParameterSelector::Auto("Colorspace".into()),
            )
            .unwrap();
        assert_eq!(parameter.oid, ParameterOid::Numeric(0x0500));
    }

    #[test]
    fn menu_context_selects_members_among_duplicate_labels() {
        let mut catalog = DeviceCatalog::new(vec![
            parameter(ParameterOid::Numeric(0x1101), "Primary Mode"),
            parameter(ParameterOid::Numeric(0x1102), "Secondary Mode"),
            parameter(ParameterOid::Numeric(0x1201), "Primary Mode"),
            parameter(ParameterOid::Numeric(0x1202), "Secondary Mode"),
            parameter(ParameterOid::Numeric(0x1203), "Offset"),
            parameter(ParameterOid::Numeric(0x1303), "Offset"),
        ])
        .unwrap();
        catalog.set_menus(
            vec![MenuGroup {
                index: 1,
                name: "CONFIGURATION".into(),
                menus: vec![
                    menu(0, "Menu Alpha", 0x1203),
                    Menu {
                        index: 1,
                        name: "Menu Beta".into(),
                        stable_id: 1,
                        layout_url: None,
                        members: vec![
                            ParameterOid::Numeric(0x1201),
                            ParameterOid::Numeric(0x1202),
                            ParameterOid::Numeric(0x1303),
                        ],
                    },
                ],
            }],
            None,
            Vec::new(),
        );

        assert!(matches!(
            catalog.resolve(&ParameterSelector::Auto("Primary Mode".into())),
            Err(ResolveError::AmbiguousDisplayName { .. })
        ));
        assert_eq!(
            catalog
                .resolve_in_menu(
                    None,
                    "Menu Beta",
                    &ParameterSelector::Auto("Primary Mode".into()),
                )
                .unwrap()
                .oid,
            ParameterOid::Numeric(0x1201)
        );
        assert_eq!(
            catalog
                .resolve_in_menu(
                    None,
                    "Menu Beta",
                    &ParameterSelector::Auto("Secondary Mode".into()),
                )
                .unwrap()
                .oid,
            ParameterOid::Numeric(0x1202)
        );
        assert_eq!(
            catalog
                .resolve_in_menu(
                    None,
                    "Menu Alpha",
                    &ParameterSelector::Auto("Offset".into()),
                )
                .unwrap()
                .oid,
            ParameterOid::Numeric(0x1203)
        );
    }

    #[test]
    fn duplicate_menu_names_require_a_group() {
        let mut catalog =
            DeviceCatalog::new(vec![parameter(ParameterOid::Numeric(1), "Parameter")]).unwrap();
        catalog.set_menus(
            vec![
                MenuGroup {
                    index: 0,
                    name: "STATUS".into(),
                    menus: vec![menu(0, "Card", 1)],
                },
                MenuGroup {
                    index: 1,
                    name: "CONFIGURATION".into(),
                    menus: vec![menu(0, "Card", 1)],
                },
            ],
            None,
            Vec::new(),
        );
        assert!(matches!(
            catalog.resolve_menu(None, "Card"),
            Err(ResolveError::AmbiguousMenu { .. })
        ));
        assert!(catalog.resolve_menu(Some("STATUS"), "Card").is_ok());
    }

    #[test]
    fn direct_oid_resolution_does_not_require_menus() {
        let catalog =
            DeviceCatalog::new(vec![parameter(ParameterOid::Numeric(0x0105), "Product")]).unwrap();
        assert_eq!(
            catalog
                .resolve(&ParameterSelector::Oid(ParameterOid::Numeric(0x0105)))
                .unwrap()
                .display_name,
            "Product"
        );
    }

    #[test]
    fn partial_menu_discovery_requires_a_complete_group_scope() {
        let mut catalog =
            DeviceCatalog::new(vec![parameter(ParameterOid::Numeric(1), "Parameter")]).unwrap();
        catalog.set_menus(
            vec![
                MenuGroup {
                    index: 0,
                    name: "STATUS".into(),
                    menus: vec![menu(0, "Card", 1)],
                },
                MenuGroup {
                    index: 1,
                    name: "CONFIGURATION".into(),
                    menus: Vec::new(),
                },
            ],
            Some("configuration menus failed".into()),
            vec![1],
        );
        assert!(matches!(
            catalog.resolve_menu(None, "Card"),
            Err(ResolveError::MenusUnavailable(_))
        ));
        assert!(catalog.resolve_menu(Some("STATUS"), "Card").is_ok());
    }

    #[test]
    fn missing_group_name_blocks_even_qualified_resolution() {
        let mut catalog =
            DeviceCatalog::new(vec![parameter(ParameterOid::Numeric(1), "Parameter")]).unwrap();
        catalog.set_menus(
            vec![MenuGroup {
                index: 0,
                name: "STATUS".into(),
                menus: vec![menu(0, "Card", 1)],
            }],
            Some("menu group 1 could not be identified".into()),
            vec![1],
        );
        assert!(matches!(
            catalog.resolve_menu(Some("STATUS"), "Card"),
            Err(ResolveError::MenusUnavailable(_))
        ));
    }

    fn menu(index: u8, name: &str, oid: u16) -> Menu {
        Menu {
            index,
            name: name.into(),
            stable_id: index.into(),
            layout_url: None,
            members: vec![ParameterOid::Numeric(oid)],
        }
    }

    fn parameter(oid: ParameterOid, display_name: &str) -> Parameter {
        Parameter {
            oid,
            display_name: display_name.into(),
            parameter_type: ParameterType::Int16,
            access: Access::ReadWrite,
            precision: None,
            widget: Widget {
                value: 0,
                known_kind: None,
            },
            role: ParameterRole::Operational,
            set_semantics: SetSemantics::Value,
            constraint: Constraint::Unconstrained,
        }
    }
}
