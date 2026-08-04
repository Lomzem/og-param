use std::collections::HashSet;
use std::io::{self, Write};
use std::process::ExitCode;
use std::str::FromStr;

use clap::{Parser, Subcommand, ValueEnum};
use og_param::OgpClient;
use og_param::client::{ClientError, Slot};
use og_param::csv_output;
use og_param::model::{
    Access, DeviceCatalog, DisplayValue, MenuGroupIdentity, MenuIdentity, Parameter,
    ParameterCandidate, ParameterOid, ParameterRole, ParameterSelector, ParameterValue,
    ResolveError, SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(version, about = "Read, write, and inspect openGear parameters")]
struct Cli {
    host: String,
    #[arg(value_parser = parse_slot)]
    slot: Slot,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn wants_json(&self) -> bool {
        match &self.command {
            Command::Info { format, .. } => *format == BasicFormat::Json,
            Command::List { format, .. } => *format == ListFormat::Json,
            Command::Read { format, .. } | Command::Write { format, .. } => {
                *format == BasicFormat::Json
            }
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show live card information.
    Info {
        /// Request a connection without displacing another client.
        #[arg(long)]
        no_force: bool,
        #[arg(long, value_enum, default_value_t)]
        format: BasicFormat,
    },
    /// List live parameters, optionally within a menu.
    List {
        /// Limit a menu name to this menu group.
        #[arg(long, requires = "menu")]
        group: Option<String>,
        /// Limit parameter selection to this menu.
        #[arg(long)]
        menu: Option<String>,
        /// Optional OID, string OID, or display name.
        #[arg(value_parser = parse_selector)]
        parameter: Option<ParameterSelector>,
        /// Request a connection without displacing another client.
        #[arg(long)]
        no_force: bool,
        #[arg(long, value_enum, default_value_t)]
        format: ListFormat,
    },
    /// Read a parameter, optionally qualified by menu and group.
    Read {
        /// Limit the menu name to this menu group.
        #[arg(long, requires = "menu")]
        group: Option<String>,
        /// Limit parameter selection to this menu.
        #[arg(long)]
        menu: Option<String>,
        /// OID, string OID, or display name.
        #[arg(value_parser = parse_selector)]
        parameter: ParameterSelector,
        /// Request a connection without displacing another client.
        #[arg(long)]
        no_force: bool,
        #[arg(long, value_enum, default_value_t)]
        format: BasicFormat,
    },
    /// Write a parameter, optionally qualified by menu and group.
    Write {
        /// Limit the menu name to this menu group.
        #[arg(long, requires = "menu")]
        group: Option<String>,
        /// Limit parameter selection to this menu.
        #[arg(long)]
        menu: Option<String>,
        /// OID, string OID, or display name.
        #[arg(value_parser = parse_selector)]
        parameter: ParameterSelector,
        /// Value or choice label to write.
        #[arg(allow_negative_numbers = true)]
        value: Option<String>,
        /// Request a connection without displacing another client.
        #[arg(long)]
        no_force: bool,
        #[arg(long, value_enum, default_value_t)]
        format: BasicFormat,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum BasicFormat {
    #[default]
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum ListFormat {
    #[default]
    Table,
    Json,
    Csv,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let wants_json = cli.wants_json();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if wants_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&error_json(&error))
                        .expect("error response is serializable")
                );
            } else {
                eprintln!("ERROR: {error}");
                print_error_guidance(&error);
            }
            ExitCode::from(error.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let Cli {
        host,
        slot,
        command,
    } = cli;
    match command {
        Command::Info { no_force, format } => {
            let mut client = OgpClient::connect(&host, slot, !no_force).await?;
            let catalog = client.discover().await?.clone();
            let info = device_info(&mut client, &catalog, host, slot).await;
            print_info(&info, format)?;
        }
        Command::List {
            group,
            menu,
            parameter,
            no_force,
            format,
        } => {
            let mut client = OgpClient::connect(&host, slot, !no_force).await?;
            let catalog = client.discover().await?;
            let parameters = match (menu.as_deref(), parameter.as_ref()) {
                (None, None) => catalog
                    .parameters
                    .iter()
                    .filter(|parameter| {
                        format != ListFormat::Table || parameter.role == ParameterRole::Operational
                    })
                    .collect(),
                (Some(menu), None) => {
                    let (_, menu) = catalog.resolve_menu(group.as_deref(), menu)?;
                    catalog.parameters_in_menu(menu)
                }
                (menu, Some(parameter)) => {
                    vec![resolve_parameter(
                        catalog,
                        group.as_deref(),
                        menu,
                        parameter,
                    )?]
                }
            };
            print_list(catalog, parameters, format)?;
        }
        Command::Read {
            group,
            menu,
            parameter,
            no_force,
            format,
        } => {
            let mut client = OgpClient::connect(&host, slot, !no_force).await?;
            let target = resolve_command_parameter(
                &mut client,
                group.as_deref(),
                menu.as_deref(),
                &parameter,
            )
            .await?;
            let result = client.read_parameter(&target).await?;
            print_read_result(result, format)?;
        }
        Command::Write {
            group,
            menu,
            parameter,
            value,
            no_force,
            format,
        } => {
            let mut client = OgpClient::connect(&host, slot, !no_force).await?;
            let target = resolve_command_parameter(
                &mut client,
                group.as_deref(),
                menu.as_deref(),
                &parameter,
            )
            .await?;
            if target.access == Access::ReadOnly {
                return Err(ClientError::ReadOnly(target.identity()).into());
            }
            let value = value.ok_or_else(|| missing_value_error(&target))?;
            let typed_value = og_param::value::parse_text(&target, &value)?;
            let result = client.write_parameter(&target, typed_value).await?;
            print_write_result(result, format)?;
        }
    }
    Ok(())
}

async fn resolve_command_parameter(
    client: &mut OgpClient,
    group: Option<&str>,
    menu: Option<&str>,
    parameter: &ParameterSelector,
) -> Result<Parameter, ClientError> {
    if group.is_none()
        && menu.is_none()
        && let ParameterSelector::Oid(oid) = parameter
    {
        return client.describe_parameter(oid).await;
    }
    Ok(resolve_parameter(client.discover().await?, group, menu, parameter)?.clone())
}

fn resolve_parameter<'a>(
    catalog: &'a DeviceCatalog,
    group: Option<&str>,
    menu: Option<&str>,
    parameter: &ParameterSelector,
) -> Result<&'a Parameter, ResolveError> {
    match menu {
        Some(menu) => catalog.resolve_in_menu(group, menu, parameter),
        None => catalog.resolve(parameter),
    }
}

#[derive(Debug, Serialize)]
struct DeviceInfo {
    host: String,
    slot: u8,
    product_name: Option<String>,
    software_revision: Option<String>,
    supplier_name: Option<String>,
    serial_number: Option<String>,
    numeric_parameters: usize,
    string_parameters: usize,
    total_parameters: usize,
    warnings: Vec<InfoWarning>,
}

#[derive(Debug, Serialize)]
struct InfoWarning {
    field: &'static str,
    oid: ParameterOid,
    message: String,
}

async fn device_info(
    client: &mut OgpClient,
    catalog: &DeviceCatalog,
    host: String,
    slot: Slot,
) -> DeviceInfo {
    let mut warnings = Vec::new();
    let product_name = read_info_field(client, catalog, "Product", 0x0105, &mut warnings).await;
    let software_revision =
        read_info_field(client, catalog, "Software", 0x010B, &mut warnings).await;
    let supplier_name = read_info_field(client, catalog, "Supplier", 0x0102, &mut warnings).await;
    let serial_number = read_info_field(client, catalog, "Serial", 0x0106, &mut warnings).await;
    DeviceInfo {
        host,
        slot: slot.get(),
        product_name,
        software_revision,
        supplier_name,
        serial_number,
        numeric_parameters: catalog
            .parameters
            .iter()
            .filter(|parameter| matches!(parameter.oid, ParameterOid::Numeric(_)))
            .count(),
        string_parameters: catalog
            .parameters
            .iter()
            .filter(|parameter| matches!(parameter.oid, ParameterOid::String(_)))
            .count(),
        total_parameters: catalog.parameters.len(),
        warnings,
    }
}

async fn read_info_field(
    client: &mut OgpClient,
    catalog: &DeviceCatalog,
    field: &'static str,
    oid: u16,
    warnings: &mut Vec<InfoWarning>,
) -> Option<String> {
    let oid = ParameterOid::Numeric(oid);
    let parameter = catalog
        .parameters
        .iter()
        .find(|parameter| parameter.oid == oid)?;
    let result = client.read_parameter(parameter).await;
    record_info_result(field, oid, result, warnings)
}

fn record_info_result(
    field: &'static str,
    oid: ParameterOid,
    result: Result<og_param::ReadResult, ClientError>,
    warnings: &mut Vec<InfoWarning>,
) -> Option<String> {
    match result {
        Ok(result) => Some(csv_output::raw_value(&result.value)),
        Err(error) => {
            warnings.push(InfoWarning {
                field,
                oid,
                message: error.to_string(),
            });
            None
        }
    }
}

fn print_info(info: &DeviceInfo, format: BasicFormat) -> Result<(), CliError> {
    match format {
        BasicFormat::Human => {
            println!(
                "Product:       {}",
                info_field_display(info, "Product", info.product_name.as_deref())
            );
            println!(
                "Software:      {}",
                info_field_display(info, "Software", info.software_revision.as_deref())
            );
            println!(
                "Supplier:      {}",
                info_field_display(info, "Supplier", info.supplier_name.as_deref())
            );
            println!(
                "Serial:        {}",
                info_field_display(info, "Serial", info.serial_number.as_deref())
            );
            println!("Host:          {}", info.host);
            println!("Slot:          {}", info.slot);
            println!("Parameters:    {}", info.total_parameters);
            println!("Numeric OIDs:  {}", info.numeric_parameters);
            println!("String OIDs:   {}", info.string_parameters);
            if !info.warnings.is_empty() {
                println!("Warnings:");
                for warning in &info.warnings {
                    println!("  {} ({}): {}", warning.field, warning.oid, warning.message);
                }
            }
        }
        BasicFormat::Json => print_json(json!({
            "schema_version": SCHEMA_VERSION,
            "ok": true,
            "operation": "info",
            "result": info,
        }))?,
    }
    Ok(())
}

fn info_field_display<'a>(info: &DeviceInfo, field: &str, value: Option<&'a str>) -> &'a str {
    value.unwrap_or_else(|| {
        if info.warnings.iter().any(|warning| warning.field == field) {
            "invalid"
        } else {
            "unknown"
        }
    })
}

fn print_read_result(result: og_param::ReadResult, format: BasicFormat) -> Result<(), CliError> {
    match format {
        BasicFormat::Human => println!(
            "{} ({}): {}",
            result.parameter.oid,
            visible_name(&result.parameter.display_name),
            display_result(&result.value, result.display_value.as_ref())
        ),
        BasicFormat::Json => print_json(json!({
            "schema_version": SCHEMA_VERSION,
            "ok": true,
            "operation": "read",
            "result": {
                "parameter": result.parameter,
                "value": value_json(&result.value),
                "display_value": result.display_value,
            }
        }))?,
    }
    Ok(())
}

fn print_write_result(result: og_param::WriteResult, format: BasicFormat) -> Result<(), CliError> {
    match format {
        BasicFormat::Human => println!(
            "{} ({}): {}",
            result.parameter.oid,
            visible_name(&result.parameter.display_name),
            display_result(&result.value, result.display_value.as_ref())
        ),
        BasicFormat::Json => print_json(json!({
            "schema_version": SCHEMA_VERSION,
            "ok": true,
            "operation": "write",
            "result": {
                "parameter": result.parameter,
                "requested_value": value_json(&result.requested_value),
                "value": value_json(&result.value),
                "display_value": result.display_value,
            }
        }))?,
    }
    Ok(())
}

fn print_list(
    catalog: &DeviceCatalog,
    parameters: Vec<&Parameter>,
    format: ListFormat,
) -> Result<(), CliError> {
    match format {
        ListFormat::Table => print!("{}", parameter_table(catalog, &parameters)),
        ListFormat::Json => print_json(json!({
            "schema_version": SCHEMA_VERSION,
            "ok": true,
            "operation": "list",
            "result": {
                "parameters": parameters
                    .iter()
                    .map(|parameter| list_parameter_json(catalog, parameter))
                    .collect::<Vec<_>>()
            },
        }))?,
        ListFormat::Csv => {
            let mut selected = DeviceCatalog::new(parameters.into_iter().cloned().collect())
                .expect("selected parameters came from a validated catalog");
            selected.set_menus(
                catalog.menus.clone(),
                catalog.menu_discovery_error.clone(),
                catalog.incomplete_menu_groups.clone(),
            );
            print_csv(&selected)?;
        }
    }
    Ok(())
}

fn list_parameter_json(catalog: &DeviceCatalog, parameter: &Parameter) -> Value {
    let mut value = serde_json::to_value(parameter).expect("parameters are serializable");
    let menus = catalog
        .menu_paths(parameter)
        .iter()
        .map(|path| json!({ "menu": path.menu, "group": path.group }))
        .collect();
    value
        .as_object_mut()
        .expect("parameters serialize as objects")
        .insert("menus".into(), Value::Array(menus));
    value
}

fn parameter_table(catalog: &DeviceCatalog, parameters: &[&Parameter]) -> String {
    let oid_width = parameters
        .iter()
        .map(|parameter| parameter.oid.to_string().chars().count())
        .fold("OID".len(), usize::max);
    let name_width = parameters
        .iter()
        .map(|parameter| visible_name(&parameter.display_name).chars().count())
        .fold("DISPLAY NAME".len(), usize::max);
    let menus: Vec<_> = parameters
        .iter()
        .map(|parameter| {
            let value = catalog
                .menu_paths(parameter)
                .iter()
                .map(csv_output::menu_path_name)
                .collect::<Vec<_>>()
                .join(", ");
            if value.is_empty() { "-".into() } else { value }
        })
        .collect();
    let menu_width = menus
        .iter()
        .map(|menu: &String| menu.chars().count())
        .fold("MENU".len(), usize::max);
    let type_width = parameters
        .iter()
        .map(|parameter| {
            csv_output::type_name(&parameter.parameter_type)
                .chars()
                .count()
        })
        .fold("TYPE".len(), usize::max);
    let mut lines = vec![format!(
        "{:<oid_width$}  {:<name_width$}  {:<menu_width$}  {:<type_width$}  ACCESS",
        "OID", "DISPLAY NAME", "MENU", "TYPE"
    )];
    for (parameter, menu) in parameters.iter().zip(menus) {
        lines.push(format!(
            "{:<oid_width$}  {:<name_width$}  {:<menu_width$}  {:<type_width$}  {}",
            parameter.oid,
            visible_name(&parameter.display_name),
            menu,
            csv_output::type_name(&parameter.parameter_type),
            csv_output::access_name(parameter.access),
        ));
        if let Some(table) = csv_output::possible_values_table(parameter) {
            lines.push("  Possible values:".into());
            lines.extend(table.lines().map(|line| format!("    {line}")));
        }
    }
    format!("{}\n", lines.join("\n"))
}

fn visible_name(value: &str) -> &str {
    if value.is_empty() { "<unnamed>" } else { value }
}

fn print_csv(catalog: &DeviceCatalog) -> Result<(), CliError> {
    let bytes = csv_output::render(catalog)?;
    io::stdout().lock().write_all(&bytes)?;
    Ok(())
}

fn display_result(value: &ParameterValue, display: Option<&DisplayValue>) -> String {
    match display {
        Some(DisplayValue::Label { value: label }) => match value {
            ParameterValue::Int16 { .. } | ParameterValue::Int32 { .. } => format!(
                "{} (raw_value={})",
                visible_name(label),
                csv_output::raw_value(value)
            ),
            _ => visible_name(label).to_owned(),
        },
        Some(DisplayValue::Aliases { values }) => format!(
            "{} (raw_value={})",
            values
                .iter()
                .map(|value| visible_name(value))
                .collect::<Vec<_>>()
                .join(" / "),
            csv_output::raw_value(value)
        ),
        Some(DisplayValue::Numeric { formatted, .. }) => formatted.clone(),
        Some(DisplayValue::Alarms { value })
            if value.active.is_empty() && value.unknown_mask == 0 =>
        {
            "No active alarms".into()
        }
        Some(DisplayValue::Alarms { value }) => {
            let mut names: Vec<_> = value
                .active
                .iter()
                .map(|alarm| alarm.name.clone())
                .collect();
            if value.unknown_mask != 0 {
                names.push(format!("unknown bits {:#X}", value.unknown_mask));
            }
            names.join(", ")
        }
        None => csv_output::raw_value(value),
    }
}

fn value_json(value: &ParameterValue) -> Value {
    match value {
        ParameterValue::Float32 { value } if !value.is_finite() => json!({
            "kind": "float32",
            "value": Value::Null,
            "special": if value.is_nan() { "nan" } else if value.is_sign_positive() { "infinity" } else { "negative_infinity" },
            "bits": format!("0x{:08X}", value.to_bits()),
        }),
        _ => serde_json::to_value(value).expect("parameter values are serializable"),
    }
}

fn print_json(value: Value) -> Result<(), CliError> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn parse_slot(input: &str) -> Result<Slot, String> {
    input
        .parse::<u8>()
        .map_err(|_| format!("invalid slot: {input}"))
        .and_then(|value| Slot::new(value).map_err(|error| error.to_string()))
}

fn parse_selector(input: &str) -> Result<ParameterSelector, String> {
    ParameterSelector::from_str(input).map_err(|error| error.to_string())
}

fn missing_value_error(parameter: &Parameter) -> CliError {
    let mut message = "missing required argument <VALUE>".to_owned();
    if let Some(possible_values) = csv_output::possible_values_table(parameter) {
        message.push_str("\npossible values:\n");
        message.push_str(&possible_values);
    }
    CliError::Usage(message)
}

fn resolution_error(error: &CliError) -> Option<&ResolveError> {
    match error {
        CliError::Client(ClientError::Resolve(error)) | CliError::Resolve(error) => Some(error),
        _ => None,
    }
}

fn connection_guidance(error: &CliError) -> Option<&'static str> {
    match error {
        CliError::Client(ClientError::ConnectionInUse) => {
            Some("Retry without --no-force to force the connection and disconnect another client.")
        }
        _ => None,
    }
}

fn print_error_guidance(error: &CliError) {
    if let Some(guidance) = connection_guidance(error) {
        eprintln!("{guidance}");
    }
    match resolution_error(error) {
        Some(ResolveError::AmbiguousDisplayName { candidates, .. }) => {
            eprintln!("{}", ambiguous_parameter_guidance(candidates));
        }
        Some(ResolveError::AmbiguousMenu { candidates, .. }) => {
            eprintln!("Select the menu with --group:");
            for candidate in candidates {
                eprintln!("  {} / {}", candidate.group, candidate.menu);
            }
        }
        Some(ResolveError::AmbiguousGroup { candidates, .. }) => {
            eprintln!("Menu groups differ only by case; select the parameter by OID:");
            for candidate in candidates {
                eprintln!("  group {}: {}", candidate.group_index, candidate.group);
            }
        }
        _ => {}
    }
}

fn ambiguous_parameter_guidance(candidates: &[ParameterCandidate]) -> String {
    let qualifier_key = |path: &og_param::model::MenuPath| {
        (
            path.menu.to_ascii_lowercase(),
            path.group_required.then(|| path.group.to_ascii_lowercase()),
        )
    };
    let unique_paths: Vec<_> = candidates
        .iter()
        .map(|candidate| {
            let mut seen = HashSet::new();
            candidate
                .menus
                .iter()
                .filter(|path| {
                    let key = qualifier_key(path);
                    let owners = candidates
                        .iter()
                        .filter(|other| {
                            other
                                .menus
                                .iter()
                                .any(|other_path| qualifier_key(other_path) == key)
                        })
                        .count();
                    owners == 1 && seen.insert(key)
                })
                .collect::<Vec<_>>()
        })
        .collect();

    if unique_paths.iter().all(Vec::is_empty) {
        let mut lines =
            vec!["No unique menu qualifier identifies these parameters; select one by OID:".into()];
        for candidate in candidates {
            let menus = candidate
                .menus
                .iter()
                .map(csv_output::menu_path_name)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "  oid:{}  {}  [{}]",
                candidate.parameter.oid,
                visible_name(&candidate.parameter.display_name),
                if menus.is_empty() { "-" } else { &menus }
            ));
        }
        return lines.join("\n");
    }

    let mut lines = vec!["Disambiguate with a unique menu qualifier or OID:".into()];
    for (candidate, paths) in candidates.iter().zip(unique_paths) {
        if paths.is_empty() {
            lines.push(format!(
                "  oid:{}  ({})",
                candidate.parameter.oid,
                visible_name(&candidate.parameter.display_name)
            ));
            continue;
        }
        for path in paths {
            if path.group_required {
                lines.push(format!(
                    "  --menu {:?} --group {:?}  (oid:{})",
                    path.menu, path.group, candidate.parameter.oid
                ));
            } else {
                lines.push(format!(
                    "  --menu {:?}  (oid:{})",
                    path.menu, candidate.parameter.oid
                ));
            }
        }
    }
    lines.push("Or select directly with oid:<OID>.".into());
    lines.join("\n")
}

fn error_json(error: &CliError) -> Value {
    let mut body = json!({
        "schema_version": SCHEMA_VERSION,
        "ok": false,
        "error": {
            "kind": error.kind(),
            "message": error.to_string(),
        }
    });
    match resolution_error(error) {
        Some(ResolveError::AmbiguousDisplayName { candidates, .. }) => {
            body["error"]["candidates"] = identities_json(candidates);
        }
        Some(ResolveError::AmbiguousMenu { candidates, .. }) => {
            body["error"]["candidates"] = menu_identities_json(candidates);
        }
        Some(ResolveError::AmbiguousGroup { candidates, .. }) => {
            body["error"]["candidates"] = group_identities_json(candidates);
        }
        _ => {}
    }
    if let Some(guidance) = connection_guidance(error) {
        body["error"]["hint"] = guidance.into();
    }
    body
}

fn identities_json(candidates: &[ParameterCandidate]) -> Value {
    serde_json::to_value(candidates).expect("parameter identities are serializable")
}

fn menu_identities_json(candidates: &[MenuIdentity]) -> Value {
    serde_json::to_value(candidates).expect("menu identities are serializable")
}

fn group_identities_json(candidates: &[MenuGroupIdentity]) -> Value {
    serde_json::to_value(candidates).expect("menu group identities are serializable")
}

#[derive(Debug, Error)]
enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    Value(#[from] og_param::value::ValueError),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    CsvOutput(#[from] csv_output::CsvOutputError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl CliError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Usage(_) => "usage",
            Self::Client(ClientError::Resolve(error)) | Self::Resolve(error) => match error {
                ResolveError::NotFound(_) | ResolveError::ParameterNotFoundInMenu { .. } => {
                    "parameter_not_found"
                }
                ResolveError::MenuNotFound { .. } => "menu_not_found",
                ResolveError::AmbiguousDisplayName { .. } => "ambiguous_parameter",
                ResolveError::AmbiguousMenu { .. } => "ambiguous_menu",
                ResolveError::AmbiguousGroup { .. } => "ambiguous_group",
                ResolveError::MenusUnavailable(_) => "menu_unavailable",
            },
            Self::Client(ClientError::AuthenticationRequired) => "authentication_required",
            Self::Client(ClientError::ConnectionInUse) => "connection_in_use",
            Self::Client(ClientError::HandshakeRejected { .. }) => "handshake_rejected",
            Self::Client(ClientError::Timeout { .. }) => "timeout",
            Self::Client(ClientError::Remote { .. }) => "remote_error",
            Self::Client(_) => "client",
            Self::Value(_) => "value",
            Self::Json(_) | Self::CsvOutput(_) | Self::Io(_) => "output",
        }
    }

    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Client(ClientError::Resolve(_)) | Self::Resolve(_) | Self::Value(_) => 4,
            Self::Client(_) => 5,
            Self::Json(_) | Self::CsvOutput(_) | Self::Io(_) => 6,
        }
    }
}

#[cfg(test)]
mod tests {
    use og_param::model::{
        Access, Choice, Constraint, KnownWidget, Menu, MenuGroup, ParameterType, SetSemantics,
        Widget,
    };

    use super::*;

    #[test]
    fn host_and_slot_precede_the_command() {
        let cli = Cli::try_parse_from(["og-param", "localhost", "1", "read", "Product"]).unwrap();
        assert_eq!(cli.host, "localhost");
        assert_eq!(cli.slot.get(), 1);
        assert!(matches!(cli.command, Command::Read { .. }));
    }

    #[test]
    fn direct_oid_uses_the_parameter_positional() {
        let cli = Cli::try_parse_from(["og-param", "localhost", "1", "read", "0x0105"]).unwrap();
        let Command::Read {
            menu, parameter, ..
        } = cli.command
        else {
            panic!("expected read command");
        };
        assert!(menu.is_none());
        assert!(matches!(
            parameter,
            ParameterSelector::Auto(value) if value == "0x0105"
        ));
    }

    #[test]
    fn explicit_oid_selects_the_targeted_path() {
        let cli =
            Cli::try_parse_from(["og-param", "localhost", "1", "read", "oid:0x1201"]).unwrap();
        let Command::Read {
            group,
            menu,
            parameter,
            ..
        } = cli.command
        else {
            panic!("expected read command");
        };
        assert!(group.is_none());
        assert!(menu.is_none());
        assert_eq!(
            parameter,
            ParameterSelector::Oid(ParameterOid::Numeric(0x1201))
        );
    }

    #[test]
    fn write_accepts_negative_values_without_option_separator() {
        let cli = Cli::try_parse_from([
            "og-param",
            "localhost",
            "1",
            "write",
            "0x0400",
            "-1",
            "--format",
            "json",
        ])
        .unwrap();
        let Command::Write { value, format, .. } = cli.command else {
            panic!("expected write command");
        };
        assert_eq!(value.as_deref(), Some("-1"));
        assert_eq!(format, BasicFormat::Json);
    }

    #[test]
    fn write_accepts_missing_value_for_metadata_aware_error() {
        let cli =
            Cli::try_parse_from(["og-param", "localhost", "1", "write", "System Genlock"]).unwrap();
        assert!(matches!(cli.command, Command::Write { value: None, .. }));
    }

    #[test]
    fn menu_option_is_valid_before_or_after_the_value() {
        for arguments in [
            vec![
                "og-param",
                "localhost",
                "1",
                "write",
                "Colorspace",
                "--menu",
                "SDI In 1",
                "Auto",
            ],
            vec![
                "og-param",
                "localhost",
                "1",
                "write",
                "Colorspace",
                "Auto",
                "--menu",
                "SDI In 1",
            ],
        ] {
            let cli = Cli::try_parse_from(arguments).unwrap();
            assert!(matches!(
                cli.command,
                Command::Write {
                    menu: Some(menu),
                    value: Some(value),
                    ..
                } if menu == "SDI In 1" && value == "Auto"
            ));
        }
    }

    #[test]
    fn missing_write_value_lists_trigger_action() {
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
        assert!(
            missing_value_error(&parameter)
                .to_string()
                .contains("| Restore | 57")
        );
    }

    #[test]
    fn malformed_info_value_becomes_a_warning() {
        let parameter = parameter().identity();
        let mut warnings = Vec::new();
        let invalid_utf8 = vec![0xFF];
        let value = record_info_result(
            "Product",
            parameter.oid.clone(),
            Err(ClientError::InvalidParameterValue {
                parameter,
                bytes: "FF 00".into(),
                source: og_param::value::ValueError::Utf8(
                    std::str::from_utf8(&invalid_utf8).unwrap_err(),
                ),
            }),
            &mut warnings,
        );
        assert!(value.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("bytes: FF 00"));
    }

    #[test]
    fn connection_in_use_error_has_actionable_output() {
        let error = CliError::Client(ClientError::ConnectionInUse);
        assert_eq!(error.kind(), "connection_in_use");
        assert_eq!(
            connection_guidance(&error),
            Some("Retry without --no-force to force the connection and disconnect another client.")
        );

        let json = error_json(&error);
        assert_eq!(json["error"]["kind"], "connection_in_use");
        assert_eq!(
            json["error"]["hint"],
            "Retry without --no-force to force the connection and disconnect another client."
        );
    }

    #[test]
    fn identical_name_and_menu_candidates_require_oid_selection() {
        let menu = og_param::model::MenuPath {
            group: "Configuration".into(),
            menu: "Input".into(),
            group_required: false,
        };
        let candidates = vec![
            ParameterCandidate {
                parameter: Parameter {
                    oid: ParameterOid::Numeric(0x1201),
                    ..parameter()
                }
                .identity(),
                menus: vec![menu.clone()],
            },
            ParameterCandidate {
                parameter: Parameter {
                    oid: ParameterOid::Numeric(0x1202),
                    ..parameter()
                }
                .identity(),
                menus: vec![menu],
            },
        ];

        let guidance = ambiguous_parameter_guidance(&candidates);

        assert!(guidance.starts_with("No unique menu qualifier"));
        assert!(guidance.contains("oid:0x1201  Parameter  [Input]"));
        assert!(guidance.contains("oid:0x1202  Parameter  [Input]"));
        assert!(!guidance.contains("--menu"));
    }

    #[test]
    fn distinct_candidate_menus_remain_actionable_qualifiers() {
        let candidates = [
            ParameterCandidate {
                parameter: Parameter {
                    oid: ParameterOid::Numeric(0x1201),
                    ..parameter()
                }
                .identity(),
                menus: vec![og_param::model::MenuPath {
                    group: "Configuration".into(),
                    menu: "Input 1".into(),
                    group_required: false,
                }],
            },
            ParameterCandidate {
                parameter: Parameter {
                    oid: ParameterOid::Numeric(0x1202),
                    ..parameter()
                }
                .identity(),
                menus: vec![og_param::model::MenuPath {
                    group: "Configuration".into(),
                    menu: "Input 2".into(),
                    group_required: false,
                }],
            },
        ];

        let guidance = ambiguous_parameter_guidance(&candidates);

        assert!(guidance.contains("--menu \"Input 1\"  (oid:0x1201)"));
        assert!(guidance.contains("--menu \"Input 2\"  (oid:0x1202)"));
    }

    #[test]
    fn possible_values_use_compact_indentation() {
        let mut parameter = parameter();
        parameter.constraint = Constraint::Choice {
            choices: vec![Choice {
                value: ParameterValue::Int16 { value: 1 },
                display_name: "On".into(),
            }],
        };
        let catalog = DeviceCatalog::new(vec![parameter.clone()]).unwrap();
        let table = parameter_table(&catalog, &[&parameter]);
        assert!(table.contains("\n  Possible values:\n    +"));
        assert!(!table.contains("                                  Possible values"));
    }

    #[test]
    fn list_outputs_include_appropriate_menu_context() {
        let first = parameter();
        let mut second = parameter();
        second.oid = ParameterOid::Numeric(2);
        let mut third = parameter();
        third.oid = ParameterOid::Numeric(3);
        let mut catalog = DeviceCatalog::new(vec![first.clone(), second.clone(), third.clone()])
            .expect("test OIDs are unique");
        catalog.set_menus(
            vec![
                MenuGroup {
                    index: 0,
                    name: "Configuration".into(),
                    menus: vec![menu(0, "Input", &first), menu(1, "System", &third)],
                },
                MenuGroup {
                    index: 1,
                    name: "Status".into(),
                    menus: vec![menu(0, "Input", &second)],
                },
            ],
            None,
            Vec::new(),
        );

        let table = parameter_table(&catalog, &[&first, &second, &third]);
        assert!(table.contains("Input (Configuration)"));
        assert!(table.contains("Input (Status)"));
        assert!(table.contains("System"));
        assert!(!table.contains("System (Configuration)"));

        let json = list_parameter_json(&catalog, &third);
        assert_eq!(
            json["menus"],
            json!([{ "menu": "System", "group": "Configuration" }])
        );
        let json = list_parameter_json(&catalog, &first);
        assert_eq!(
            json["menus"],
            json!([{ "menu": "Input", "group": "Configuration" }])
        );
    }

    #[test]
    fn numeric_choice_results_include_the_raw_value() {
        assert_eq!(
            display_result(
                &ParameterValue::Int16 { value: 4 },
                Some(&DisplayValue::Label {
                    value: "Free Run".into(),
                }),
            ),
            "Free Run (raw_value=4)"
        );
        assert_eq!(
            display_result(
                &ParameterValue::String {
                    value: "Auto".into(),
                },
                Some(&DisplayValue::Label {
                    value: "Auto".into(),
                }),
            ),
            "Auto"
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

    fn menu(index: u8, name: &str, parameter: &Parameter) -> Menu {
        Menu {
            index,
            name: name.into(),
            stable_id: index.into(),
            layout_url: None,
            members: vec![parameter.oid.clone()],
        }
    }
}
