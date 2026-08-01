use crate::model::{
    Access, Constraint, DeviceCatalog, Parameter, ParameterRole, ParameterType, ParameterValue,
};
use thiserror::Error;

pub fn current(catalog: &DeviceCatalog) -> Result<Vec<u8>, CsvOutputError> {
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    writer.write_record([
        "OID",
        "Display Name",
        "Menu",
        "Type",
        "Access",
        "Role",
        "Constraint",
        "Possible Values",
    ])?;
    for parameter in &catalog.parameters {
        writer.write_record([
            parameter.oid.to_string(),
            parameter.display_name.clone(),
            catalog
                .menu_paths(parameter)
                .iter()
                .map(menu_path_name)
                .collect::<Vec<_>>()
                .join(" | "),
            type_name(&parameter.parameter_type),
            access_name(parameter.access).into(),
            role_name(parameter.role).into(),
            constraint_name(&parameter.constraint).into(),
            possible_values(parameter).join(" | "),
        ])?;
    }
    writer
        .into_inner()
        .map_err(|error| CsvOutputError::Finalize(error.error().to_string()))
}

pub fn menu_path_name(path: &crate::model::MenuPath) -> String {
    if path.group_required {
        format!("{} ({})", path.menu, path.group)
    } else {
        path.menu.clone()
    }
}

pub fn legacy(catalog: &DeviceCatalog) -> Result<Vec<u8>, CsvOutputError> {
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    writer.write_record(["Parameter Name", "Dashboard Parameter Name", "Value Names"])?;
    for parameter in catalog
        .parameters
        .iter()
        .filter(|parameter| parameter.role == ParameterRole::Operational)
    {
        writer.write_record([
            parameter.oid.to_string(),
            parameter.display_name.clone(),
            String::new(),
        ])?;
        for value in legacy_value_names(parameter) {
            writer.write_record(["", "", &format!(" {value}")])?;
        }
    }
    writer
        .into_inner()
        .map_err(|error| CsvOutputError::Finalize(error.error().to_string()))
}

fn legacy_value_names(parameter: &Parameter) -> Vec<String> {
    match parameter.resolved_constraint() {
        Constraint::Choice { choices } => choices
            .iter()
            .map(|choice| choice.display_name.clone())
            .collect(),
        Constraint::StringChoice { choices, .. } => choices.clone(),
        Constraint::AlarmTable { alarms } => {
            alarms.iter().map(|alarm| alarm.name.clone()).collect()
        }
        _ => Vec::new(),
    }
}

pub fn possible_values(parameter: &Parameter) -> Vec<String> {
    match parameter.resolved_constraint() {
        Constraint::Choice { choices } => choices
            .iter()
            .map(|choice| format!("{}={}", choice.display_name, raw_value(&choice.value)))
            .collect(),
        Constraint::StringChoice { choices, .. } => choices.clone(),
        Constraint::AlarmTable { alarms } => alarms
            .iter()
            .map(|alarm| format!("{}=bit {}", alarm.name, alarm.bit))
            .collect(),
        Constraint::Range {
            minimum,
            maximum,
            step,
            ..
        } => vec![format!(
            "{}..{}{}",
            raw_value(minimum),
            raw_value(maximum),
            step.as_ref()
                .map(|step| format!(" step {}", raw_value(step)))
                .unwrap_or_default()
        )],
        _ => Vec::new(),
    }
}

pub fn possible_values_table(parameter: &Parameter) -> Option<String> {
    let (headers, rows) = match parameter.resolved_constraint() {
        Constraint::Choice { choices }
            if parameter.set_semantics == crate::model::SetSemantics::Trigger
                && matches!(choices.len(), 1 | 2) =>
        {
            let display = &choices[0];
            let action = choices.last().expect("trigger has one or two choices");
            (
                vec!["BUTTON", "ACTION RAW VALUE"],
                vec![vec![
                    visible_cell(&display.display_name),
                    raw_value(&action.value),
                ]],
            )
        }
        Constraint::Choice { choices } => (
            vec!["VALUE", "RAW VALUE"],
            choices
                .iter()
                .map(|choice| vec![visible_cell(&choice.display_name), raw_value(&choice.value)])
                .collect(),
        ),
        Constraint::StringChoice { choices, .. } => (
            vec!["VALUE"],
            choices
                .iter()
                .map(|choice| vec![visible_cell(choice)])
                .collect(),
        ),
        Constraint::AlarmTable { alarms } => (
            vec!["VALUE", "BIT"],
            alarms
                .iter()
                .map(|alarm| vec![visible_cell(&alarm.name), alarm.bit.to_string()])
                .collect(),
        ),
        Constraint::Range {
            minimum,
            maximum,
            step,
            ..
        } => (
            vec!["MINIMUM", "MAXIMUM", "STEP"],
            vec![vec![
                raw_value(minimum),
                raw_value(maximum),
                step.as_ref().map(raw_value).unwrap_or_default(),
            ]],
        ),
        _ => return None,
    };
    Some(ascii_table(&headers, &rows))
}

fn visible_cell(value: &str) -> String {
    if value.is_empty() {
        "<empty>".into()
    } else if value.trim() != value {
        format!("{value:?}")
    } else {
        value.to_owned()
    }
}

fn ascii_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| escape_cell(cell)).collect())
        .collect();
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(column, header)| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.chars().count())
                .fold(header.chars().count(), usize::max)
        })
        .collect();
    let border = format!(
        "+{}+",
        widths
            .iter()
            .map(|width| "-".repeat(width + 2))
            .collect::<Vec<_>>()
            .join("+")
    );
    let format_row = |cells: &[String]| {
        format!(
            "| {} |",
            cells
                .iter()
                .zip(&widths)
                .map(|(cell, width)| format!(
                    "{}{}",
                    cell,
                    " ".repeat(width - cell.chars().count())
                ))
                .collect::<Vec<_>>()
                .join(" | ")
        )
    };
    let header_cells: Vec<String> = headers.iter().map(|header| (*header).to_owned()).collect();
    let mut lines = vec![border.clone(), format_row(&header_cells), border.clone()];
    lines.extend(rows.iter().map(|row| format_row(row)));
    lines.push(border);
    lines.join("\n")
}

fn escape_cell(value: &str) -> String {
    value.replace('\r', "\\r").replace('\n', "\\n")
}

pub fn raw_value(value: &ParameterValue) -> String {
    match value {
        ParameterValue::Int16 { value } => value.to_string(),
        ParameterValue::Int32 { value } => value.to_string(),
        ParameterValue::Float32 { value } => value.to_string(),
        ParameterValue::String { value } => value.clone(),
    }
}

pub fn type_name(value: &ParameterType) -> String {
    match value {
        ParameterType::Int16 => "int16".into(),
        ParameterType::Int32 => "int32".into(),
        ParameterType::Float32 => "float32".into(),
        ParameterType::String { .. } => "string".into(),
        ParameterType::Int16Array { .. } => "int16_array".into(),
        ParameterType::Int32Array { .. } => "int32_array".into(),
        ParameterType::Float32Array { .. } => "float32_array".into(),
        ParameterType::StringArray { .. } => "string_array".into(),
        ParameterType::Binary => "binary".into(),
        ParameterType::Unsupported { type_id } => format!("type_{type_id}"),
    }
}

pub fn access_name(value: Access) -> &'static str {
    match value {
        Access::ReadOnly => "read_only",
        Access::ReadWrite => "read_write",
    }
}

fn role_name(value: ParameterRole) -> &'static str {
    match value {
        ParameterRole::Operational => "operational",
        ParameterRole::Layout => "layout",
    }
}

fn constraint_name(value: &Constraint) -> &'static str {
    match value {
        Constraint::Unconstrained => "unconstrained",
        Constraint::Choice { .. } => "choice",
        Constraint::StringChoice { .. } => "string_choice",
        Constraint::Range { .. } => "range",
        Constraint::AlarmTable { .. } => "alarm_table",
        Constraint::External { .. } => "external_resolved",
        Constraint::Unsupported { .. } => "unsupported",
    }
}

#[derive(Debug, Error)]
pub enum CsvOutputError {
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error("failed to finalize CSV: {0}")]
    Finalize(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Choice, KnownWidget, Menu, MenuGroup, ParameterOid, SetSemantics, Widget};

    #[test]
    fn choice_table_includes_labels_and_values() {
        let mut parameter = parameter();
        parameter.constraint = Constraint::Choice {
            choices: vec![
                Choice {
                    value: ParameterValue::Int16 { value: 0 },
                    display_name: "Off".into(),
                },
                Choice {
                    value: ParameterValue::Int16 { value: 1 },
                    display_name: "On".into(),
                },
            ],
        };
        assert_eq!(
            possible_values_table(&parameter).unwrap(),
            "+-------+-----------+\n| VALUE | RAW VALUE |\n+-------+-----------+\n| Off   | 0         |\n| On    | 1         |\n+-------+-----------+"
        );
    }

    #[test]
    fn current_csv_includes_conditional_menu_context() {
        let parameter = parameter();
        let mut catalog = DeviceCatalog::new(vec![parameter.clone()]).unwrap();
        catalog.set_menus(
            vec![MenuGroup {
                index: 0,
                name: "Configuration".into(),
                menus: vec![Menu {
                    index: 0,
                    name: "Input".into(),
                    stable_id: 0,
                    layout_url: None,
                    members: vec![parameter.oid.clone()],
                }],
            }],
            None,
            Vec::new(),
        );

        let output = String::from_utf8(current(&catalog).unwrap()).unwrap();
        assert!(output.starts_with("OID,Display Name,Menu,Type,"));
        assert!(output.contains("0x0001,Parameter,Input,int16,"));
        assert!(!output.contains("Input (Configuration)"));
    }

    fn parameter() -> Parameter {
        Parameter {
            oid: ParameterOid::Numeric(1),
            display_name: "Parameter".into(),
            parameter_type: ParameterType::Int16,
            access: Access::ReadWrite,
            precision: None,
            widget: Widget {
                value: 7,
                known_kind: Some(KnownWidget::Choice),
            },
            role: ParameterRole::Operational,
            set_semantics: SetSemantics::Value,
            constraint: Constraint::Unconstrained,
        }
    }
}
