use thiserror::Error;

use crate::csv_output;
use crate::model::{Constraint, Parameter, ParameterType, ParameterValue, label_eq};

pub fn decode(parameter: &Parameter, bytes: &[u8]) -> Result<ParameterValue, ValueError> {
    match parameter.parameter_type {
        ParameterType::Int16 => exact(bytes, 2).map(|bytes| ParameterValue::Int16 {
            value: i16::from_be_bytes([bytes[0], bytes[1]]),
        }),
        ParameterType::Int32 => exact(bytes, 4).map(|bytes| ParameterValue::Int32 {
            value: i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }),
        ParameterType::Float32 => exact(bytes, 4).map(|bytes| ParameterValue::Float32 {
            value: f32::from_bits(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        }),
        ParameterType::String { .. } => {
            if bytes.is_empty() || bytes.last() != Some(&0) {
                return Err(ValueError::UnterminatedString);
            }
            if bytes[..bytes.len() - 1].contains(&0) {
                return Err(ValueError::EmbeddedNul);
            }
            let value = std::str::from_utf8(&bytes[..bytes.len() - 1])?;
            Ok(ParameterValue::String {
                value: value.to_owned(),
            })
        }
        _ => Err(ValueError::UnsupportedType(
            parameter.parameter_type.clone(),
        )),
    }
}

pub fn encode(parameter: &Parameter, value: &ParameterValue) -> Result<Vec<u8>, ValueError> {
    if !parameter.parameter_type.accepts(value) {
        return Err(ValueError::TypeMismatch);
    }
    validate_constraint(parameter, value)?;
    match value {
        ParameterValue::Int16 { value } => Ok(value.to_be_bytes().to_vec()),
        ParameterValue::Int32 { value } => Ok(value.to_be_bytes().to_vec()),
        ParameterValue::Float32 { value } => {
            if !value.is_finite() {
                return Err(ValueError::NonFiniteFloat);
            }
            Ok(value.to_bits().to_be_bytes().to_vec())
        }
        ParameterValue::String { value } => {
            if value.as_bytes().contains(&0) {
                return Err(ValueError::EmbeddedNul);
            }
            if let ParameterType::String {
                max_bytes: Some(maximum),
            } = parameter.parameter_type
                && value.len() > maximum as usize
            {
                return Err(ValueError::StringTooLong {
                    maximum: maximum as usize,
                    actual: value.len(),
                });
            }
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            Ok(bytes)
        }
    }
}

pub fn parse_text(parameter: &Parameter, text: &str) -> Result<ParameterValue, ValueError> {
    let mut trigger_action = None;
    if let Constraint::Choice { choices } = parameter.resolved_constraint() {
        if parameter.set_semantics == crate::model::SetSemantics::Trigger {
            trigger_action = match choices.as_slice() {
                [choice] => Some(choice),
                [_, action] => Some(action),
                _ => None,
            };
            if let Some(action) = trigger_action
                && choices
                    .iter()
                    .any(|choice| label_eq(&choice.display_name, text))
            {
                return Ok(action.value.clone());
            }
        }
        if trigger_action.is_none() {
            let matches = choices
                .iter()
                .filter(|choice| label_eq(&choice.display_name, text))
                .collect::<Vec<_>>();
            if let Some(first) = matches.first() {
                if matches.iter().any(|choice| choice.value != first.value) {
                    return Err(ValueError::AmbiguousChoice(text.to_owned()));
                }
                return Ok(first.value.clone());
            }
        }
    }

    let value = match parameter.parameter_type {
        ParameterType::Int16 => ParameterValue::Int16 {
            value: parse_i16(text).map_err(|_| invalid_text(parameter, text))?,
        },
        ParameterType::Int32 => ParameterValue::Int32 {
            value: parse_i32(text).map_err(|_| invalid_text(parameter, text))?,
        },
        ParameterType::Float32 => ParameterValue::Float32 {
            value: text.parse().map_err(|_| invalid_text(parameter, text))?,
        },
        ParameterType::String { .. } => {
            let value = match parameter.resolved_constraint() {
                Constraint::StringChoice { choices, .. } => {
                    let matches = choices
                        .iter()
                        .filter(|choice| label_eq(choice, text))
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [] => text.to_owned(),
                        [choice] => (*choice).clone(),
                        [first, rest @ ..] if rest.iter().all(|choice| *choice == *first) => {
                            (*first).clone()
                        }
                        _ => return Err(ValueError::AmbiguousChoice(text.to_owned())),
                    }
                }
                _ => text.to_owned(),
            };
            ParameterValue::String { value }
        }
        _ => {
            return Err(ValueError::UnsupportedType(
                parameter.parameter_type.clone(),
            ));
        }
    };
    if let Some(action) = trigger_action
        && value != action.value
    {
        return Err(invalid_text(parameter, text));
    }
    validate_constraint(parameter, &value).map_err(|error| match error {
        ValueError::ConstraintViolation => invalid_text(parameter, text),
        error => error,
    })?;
    Ok(value)
}

fn invalid_text(parameter: &Parameter, text: &str) -> ValueError {
    if let Some(possible_values) = csv_output::possible_values_table(parameter) {
        ValueError::InvalidParameterValue {
            value: text.to_owned(),
            possible_values,
        }
    } else {
        ValueError::InvalidText(text.to_owned())
    }
}

fn validate_constraint(parameter: &Parameter, value: &ParameterValue) -> Result<(), ValueError> {
    match &parameter.constraint {
        Constraint::Unconstrained | Constraint::AlarmTable { .. } => Ok(()),
        Constraint::Choice { choices } => choices
            .iter()
            .any(|choice| choice.value == *value)
            .then_some(())
            .ok_or(ValueError::ConstraintViolation),
        Constraint::StringChoice {
            choices,
            arbitrary_values_allowed,
        } => match value {
            ParameterValue::String { value }
                if *arbitrary_values_allowed || choices.contains(value) =>
            {
                Ok(())
            }
            _ => Err(ValueError::ConstraintViolation),
        },
        Constraint::Range {
            minimum,
            maximum,
            step,
            ..
        } => {
            let value = value.number().ok_or(ValueError::ConstraintViolation)?;
            let minimum = minimum.number().ok_or(ValueError::ConstraintViolation)?;
            let maximum = maximum.number().ok_or(ValueError::ConstraintViolation)?;
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(ValueError::ConstraintViolation);
            }
            if let Some(step) = step.as_ref().and_then(ParameterValue::number) {
                let quotient = (value - minimum) / step;
                if (quotient - quotient.round()).abs() > 1e-6 {
                    return Err(ValueError::ConstraintViolation);
                }
            }
            Ok(())
        }
        Constraint::External { resolved, .. } => {
            let mut parameter = parameter.clone();
            parameter.constraint = *resolved.clone();
            validate_constraint(&parameter, value)
        }
        Constraint::Unsupported { .. } => Err(ValueError::UnsupportedConstraint),
    }
}

fn exact(bytes: &[u8], expected: usize) -> Result<&[u8], ValueError> {
    if bytes.len() == expected {
        Ok(bytes)
    } else {
        Err(ValueError::InvalidLength {
            expected,
            actual: bytes.len(),
        })
    }
}

fn parse_i16(input: &str) -> Result<i16, ValueError> {
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16)
            .map(|value| i16::from_be_bytes(value.to_be_bytes()))
            .map_err(|_| ValueError::InvalidText(input.to_owned()))
    } else {
        input
            .parse()
            .map_err(|_| ValueError::InvalidText(input.to_owned()))
    }
}

fn parse_i32(input: &str) -> Result<i32, ValueError> {
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
            .map(|value| i32::from_be_bytes(value.to_be_bytes()))
            .map_err(|_| ValueError::InvalidText(input.to_owned()))
    } else {
        input
            .parse()
            .map_err(|_| ValueError::InvalidText(input.to_owned()))
    }
}

#[derive(Debug, Error)]
pub enum ValueError {
    #[error("expected {expected} value bytes, received {actual}")]
    InvalidLength { expected: usize, actual: usize },
    #[error("string value is not NUL-terminated")]
    UnterminatedString,
    #[error("string value contains an embedded NUL")]
    EmbeddedNul,
    #[error("string is too long: maximum {maximum} UTF-8 bytes, received {actual}")]
    StringTooLong { maximum: usize, actual: usize },
    #[error("value is not valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("parameter type is not supported at runtime: {0:?}")]
    UnsupportedType(ParameterType),
    #[error("value does not match parameter type")]
    TypeMismatch,
    #[error("invalid value: {0}")]
    InvalidText(String),
    #[error("invalid value: {value}\npossible values:\n{possible_values}")]
    InvalidParameterValue {
        value: String,
        possible_values: String,
    },
    #[error("non-finite float writes are not supported")]
    NonFiniteFloat,
    #[error("value violates the parameter constraint")]
    ConstraintViolation,
    #[error("choice label is ambiguous: {0}")]
    AmbiguousChoice(String),
    #[error("constraint type is unsupported; safe writes are unavailable")]
    UnsupportedConstraint,
}

#[cfg(test)]
mod tests {
    use crate::model::{
        Access, Choice, Constraint, KnownWidget, ParameterOid, ParameterRole, SetSemantics, Widget,
    };

    use super::*;

    #[test]
    fn duplicate_choice_labels_with_different_values_are_ambiguous() {
        let mut parameter = parameter();
        parameter.constraint = Constraint::Choice {
            choices: vec![
                Choice {
                    value: ParameterValue::Int16 { value: 1 },
                    display_name: "Duplicate".into(),
                },
                Choice {
                    value: ParameterValue::Int16 { value: 2 },
                    display_name: "Duplicate".into(),
                },
            ],
        };
        assert!(matches!(
            parse_text(&parameter, "Duplicate"),
            Err(ValueError::AmbiguousChoice(_))
        ));
    }

    #[test]
    fn choice_labels_ignore_ascii_case() {
        let mut parameter = parameter();
        parameter.constraint = Constraint::Choice {
            choices: vec![Choice {
                value: ParameterValue::Int16 { value: 4 },
                display_name: "Free Run".into(),
            }],
        };
        assert_eq!(
            parse_text(&parameter, "free run").unwrap(),
            ParameterValue::Int16 { value: 4 }
        );
    }

    #[test]
    fn string_choice_uses_the_canonical_device_spelling() {
        let mut parameter = parameter();
        parameter.parameter_type = ParameterType::String {
            max_bytes: Some(32),
        };
        parameter.constraint = Constraint::StringChoice {
            choices: vec!["Auto".into()],
            arbitrary_values_allowed: true,
        };
        assert_eq!(
            parse_text(&parameter, "auto").unwrap(),
            ParameterValue::String {
                value: "Auto".into()
            }
        );
        assert_eq!(
            parse_text(&parameter, "custom").unwrap(),
            ParameterValue::String {
                value: "custom".into()
            }
        );
    }

    #[test]
    fn string_choices_differing_only_by_case_are_ambiguous() {
        let mut parameter = parameter();
        parameter.parameter_type = ParameterType::String {
            max_bytes: Some(32),
        };
        parameter.constraint = Constraint::StringChoice {
            choices: vec!["Auto".into(), "AUTO".into()],
            arbitrary_values_allowed: true,
        };
        assert!(matches!(
            parse_text(&parameter, "auto"),
            Err(ValueError::AmbiguousChoice(_))
        ));
    }

    #[test]
    fn invalid_choice_lists_all_possible_values() {
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

        let error = parse_text(&parameter, "invalid").unwrap_err();

        assert_eq!(
            error.to_string(),
            concat!(
                "invalid value: invalid\n",
                "possible values:\n",
                "+-------+-----------+\n",
                "| VALUE | RAW VALUE |\n",
                "+-------+-----------+\n",
                "| Off   | 0         |\n",
                "| On    | 1         |\n",
                "+-------+-----------+"
            )
        );
    }

    #[test]
    fn disallowed_numeric_choice_lists_all_possible_values() {
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

        let error = parse_text(&parameter, "2").unwrap_err();

        assert_eq!(
            error.to_string(),
            concat!(
                "invalid value: 2\n",
                "possible values:\n",
                "+-------+-----------+\n",
                "| VALUE | RAW VALUE |\n",
                "+-------+-----------+\n",
                "| Off   | 0         |\n",
                "| On    | 1         |\n",
                "+-------+-----------+"
            )
        );
    }

    #[test]
    fn external_choice_labels_are_accepted() {
        let mut parameter = parameter();
        parameter.constraint = Constraint::External {
            object_id: 7,
            resolved: Box::new(Constraint::Choice {
                choices: vec![Choice {
                    value: ParameterValue::Int16 { value: 1 },
                    display_name: "On".into(),
                }],
            }),
        };
        assert_eq!(
            parse_text(&parameter, "On").unwrap(),
            ParameterValue::Int16 { value: 1 }
        );
    }

    #[test]
    fn two_choice_trigger_label_selects_the_action_value() {
        let mut parameter = parameter();
        parameter.set_semantics = SetSemantics::Trigger;
        parameter.constraint = Constraint::Choice {
            choices: vec![
                Choice {
                    value: ParameterValue::Int16 { value: 0 },
                    display_name: "Restore".into(),
                },
                Choice {
                    value: ParameterValue::Int16 { value: 57 },
                    display_name: "Restore".into(),
                },
            ],
        };
        assert_eq!(
            parse_text(&parameter, "Restore").unwrap(),
            ParameterValue::Int16 { value: 57 }
        );
        assert!(parse_text(&parameter, "0").is_err());
        assert_eq!(
            parse_text(&parameter, "57").unwrap(),
            ParameterValue::Int16 { value: 57 }
        );
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
