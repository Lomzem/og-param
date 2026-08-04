# Scripting with og-param

`og-param` provides structured output for scripts. JSON is the preferred format because every command supports it and runtime failures use a JSON error envelope.

## Output Formats

| Command | JSON | CSV | Human-readable default |
| --- | --- | --- | --- |
| `info` | Yes | No | Text |
| `list` | Yes | Yes | Table |
| `read` | Yes | No | Text |
| `write` | Yes | No | Text |

## Usage

Select JSON with `--format json`:

```sh
og-param 192.0.2.10 1 info --format json
og-param 192.0.2.10 1 list --format json
og-param 192.0.2.10 1 read "Operating Mode" --format json
og-param 192.0.2.10 1 write "Operating Mode" "Enabled" --format json
```

Select CSV for `list` with `--format csv`.

```sh
og-param 192.0.2.10 1 list --format csv
```

Structured output is written to standard output. Human-readable errors are written to standard error. When JSON output is selected, runtime errors are written as JSON to standard output and the process returns a nonzero exit status.

## JSON Contract

The current JSON schema version is `6`. Scripts should check `schema_version` and reject versions they do not support.

Each successful response has this envelope:

```json
{
  "schema_version": 6,
  "ok": true,
  "operation": "info",
  "result": {}
}
```

The `operation` value is one of `info`, `list`, `read`, or `write`. The content of `result` depends on the operation.

### Info Result

```json
{
  "schema_version": 6,
  "ok": true,
  "operation": "info",
  "result": {
    "host": "192.0.2.10",
    "slot": 1,
    "product_name": "Example Device",
    "software_revision": "1.2.3",
    "supplier_name": "Example Supplier",
    "serial_number": "ABC123",
    "numeric_parameters": 120,
    "string_parameters": 5,
    "total_parameters": 125,
    "warnings": []
  }
}
```

| Field | JSON type | Meaning |
| --- | --- | --- |
| `host` | string | Host supplied on the command line. |
| `slot` | integer | Card slot supplied on the command line. |
| `product_name` | string or `null` | Product name, if available. |
| `software_revision` | string or `null` | Software revision, if available. |
| `supplier_name` | string or `null` | Supplier name, if available. |
| `serial_number` | string or `null` | Serial number, if available. |
| `numeric_parameters` | integer | Number of discovered numeric OIDs. |
| `string_parameters` | integer | Number of discovered string OIDs. |
| `total_parameters` | integer | Total number of discovered parameters. |
| `warnings` | array | Problems that occurred while reading optional identity fields. |

An info warning has this form:

```json
{
  "field": "Product",
  "oid": "0x0105",
  "message": "error description"
}
```

An unavailable identity field is `null`. A warning can be present while `ok` remains `true` because parameter discovery completed.

### List Result

The list result contains a `parameters` array:

```json
{
  "schema_version": 6,
  "ok": true,
  "operation": "list",
  "result": {
    "parameters": [
      {
        "oid": "0x0400",
        "display_name": "Operating Mode",
        "parameter_type": "int16",
        "access": "read_write",
        "precision": null,
        "widget": {
          "value": 7,
          "known_kind": "choice"
        },
        "role": "operational",
        "set_semantics": "value",
        "constraint": {
          "kind": "choice",
          "choices": [
            {
              "value": {
                "kind": "int16",
                "value": 0
              },
              "display_name": "Disabled"
            },
            {
              "value": {
                "kind": "int16",
                "value": 1
              },
              "display_name": "Enabled"
            }
          ]
        },
        "menus": [
          {
            "menu": "System",
            "group": "Configuration"
          }
        ]
      }
    ]
  }
}
```

Each parameter contains these fields:

| Field | JSON type | Meaning |
| --- | --- | --- |
| `oid` | string | Numeric OID formatted as `0xNNNN`, or the string OID as received. |
| `display_name` | string | Device-provided display name. It can be empty. |
| `parameter_type` | string or object | Raw parameter type. See [Parameter types](#parameter-types). |
| `access` | string | `read_only` or `read_write`. |
| `precision` | integer or `null` | Display precision from the descriptor. |
| `widget` | object | Raw widget value and an optional known widget name. |
| `role` | string | `operational` or `layout`. |
| `set_semantics` | string | `value` or `trigger`. |
| `constraint` | object | Allowed or displayed values. See [Constraints](#constraints). |
| `menus` | array | Menu paths that contain the parameter. |

`widget.known_kind` is `null` for an unknown widget. A known value is one of `button`, `choice`, `text`, `checkbox`, `spinner`, `slider`, `alarm`, `hidden`, `title`, or `rich_label`.

A menu path always contains both `menu` and `group`:

```json
{
  "menu": "Input",
  "group": "Configuration"
}
```

An unfiltered JSON list includes both operational and layout parameters. This differs from the default table, which shows only operational parameters. Menu (`--menu`) and parameter selectors still limit the JSON list in the same way that they limit the table.

#### Parameter Types

Scalar and marker types are JSON strings:

```json
"int16"
"int32"
"float32"
"binary"
```

Types with metadata are single-key objects:

```json
{ "string": { "max_bytes": 64 } }
{ "int16_array": { "length": 8 } }
{ "int32_array": { "length": null } }
{ "float32_array": { "length": 4 } }
{ "string_array": { "length": 4, "max_element_bytes": 32 } }
{ "unsupported": { "type_id": 99 } }
```

Metadata values can be `null` when the device does not provide a limit. Scalar integers, floats, and strings support runtime reads and writes. Arrays, binary values, and unsupported types are list-only.

#### Constraints

Every constraint has a `kind` discriminator.

Unconstrained:

```json
{ "kind": "unconstrained" }
```

Numeric choice:

```json
{
  "kind": "choice",
  "choices": [
    {
      "value": { "kind": "int16", "value": 1 },
      "display_name": "Enabled"
    }
  ]
}
```

String choice:

```json
{
  "kind": "string_choice",
  "choices": ["Automatic", "Manual"],
  "arbitrary_values_allowed": false
}
```

Range:

```json
{
  "kind": "range",
  "minimum": { "kind": "int16", "value": 0 },
  "maximum": { "kind": "int16", "value": 100 },
  "display_minimum": { "kind": "int16", "value": 0 },
  "display_maximum": { "kind": "int16", "value": 10 },
  "step": { "kind": "int16", "value": 1 }
}
```

`display_minimum`, `display_maximum`, and `step` can be `null`.

Alarm table:

```json
{
  "kind": "alarm_table",
  "alarms": [
    {
      "bit": 0,
      "name": "Signal Missing",
      "severity_value": 2
    }
  ]
}
```

Resolved external constraint:

```json
{
  "kind": "external",
  "object_id": 28673,
  "resolved": {
    "kind": "choice",
    "choices": []
  }
}
```

Unsupported constraint:

```json
{
  "kind": "unsupported",
  "type_id": 99
}
```

### Read Result

```json
{
  "schema_version": 6,
  "ok": true,
  "operation": "read",
  "result": {
    "parameter": {
      "oid": "0x0400",
      "display_name": "Operating Mode"
    },
    "value": {
      "kind": "int16",
      "value": 1
    },
    "display_value": {
      "kind": "label",
      "value": "Enabled"
    }
  }
}
```

`value` is the raw value returned by the device. `display_value` is interpreted from the parameter constraint. It is `null` when no display interpretation is available.

### Write Result

```json
{
  "schema_version": 6,
  "ok": true,
  "operation": "write",
  "result": {
    "parameter": {
      "oid": "0x0400",
      "display_name": "Operating Mode"
    },
    "requested_value": {
      "kind": "int16",
      "value": 1
    },
    "value": {
      "kind": "int16",
      "value": 1
    },
    "display_value": {
      "kind": "label",
      "value": "Enabled"
    }
  }
}
```

`requested_value` is the parsed value sent to the device. `value` is the value in the device response. Scripts should use `value` when they need the confirmed result.

### Parameter Values

Raw parameter values use a `kind` discriminator:

```json
{ "kind": "int16", "value": -1 }
{ "kind": "int32", "value": 100000 }
{ "kind": "float32", "value": 1.25 }
{ "kind": "string", "value": "Example" }
```

JSON cannot directly represent non-finite floats. A non-finite `float32` returned by a read or write uses `null`, a classification, and the original bits:

```json
{
  "kind": "float32",
  "value": null,
  "special": "nan",
  "bits": "0x7FC00000"
}
```

`special` is `nan`, `infinity`, or `negative_infinity`.

### Display Values

A display value is one of these forms.

Choice label:

```json
{ "kind": "label", "value": "Enabled" }
```

Multiple labels for one raw value:

```json
{ "kind": "aliases", "values": ["Primary", "Alternate"] }
```

Mapped numeric display value:

```json
{ "kind": "numeric", "value": 5.5, "formatted": "5.50" }
```

Decoded alarms:

```json
{
  "kind": "alarms",
  "value": {
    "active": [
      {
        "bit": 0,
        "mask": 1,
        "name": "Signal Missing",
        "severity_value": 2
      }
    ],
    "unknown_mask": 4
  }
}
```

`unknown_mask` contains active bits that are not defined by the alarm table.

## JSON Errors

When `--format json` is selected and command execution fails, the response has this form:

```json
{
  "schema_version": 6,
  "ok": false,
  "error": {
    "kind": "parameter_not_found",
    "message": "error description"
  }
}
```

Error responses do not contain `operation` or `result`.

`error.kind` is one of:

| Kind | Meaning |
| --- | --- |
| `usage` | The command combination or required value is invalid after parsing. |
| `parameter_not_found` | The parameter does not exist in the selected scope. |
| `menu_not_found` | The selected menu does not exist. |
| `ambiguous_parameter` | More than one parameter matches the selector. |
| `ambiguous_menu` | More than one menu matches the selector. |
| `ambiguous_group` | More than one group matches the selector. |
| `menu_unavailable` | Required menu metadata could not be discovered. |
| `authentication_required` | The connection requires unsupported authentication. |
| `connection_in_use` | A non-forced connection was refused because all connections are in use. |
| `handshake_rejected` | The device rejected the connection handshake. |
| `timeout` | A connection or request phase timed out. |
| `remote_error` | The device returned an OGP error. |
| `client` | Another connection, protocol, discovery, or runtime parameter error occurred. |
| `value` | A supplied value could not be parsed or did not satisfy its constraint. |
| `output` | JSON, CSV, or standard-output writing failed. |

Some errors add fields to the `error` object.

An ambiguous parameter adds `candidates`:

```json
{
  "kind": "ambiguous_parameter",
  "message": "error description",
  "candidates": [
    {
      "parameter": {
        "oid": "0x1201",
        "display_name": "Status"
      },
      "menus": [
        {
          "group": "Configuration",
          "menu": "Input"
        }
      ]
    }
  ]
}
```

An ambiguous menu candidate contains `group`, `menu`, `group_index`, and `menu_index`. An ambiguous group candidate contains `group` and `group_index`.

A `connection_in_use` error also contains a `hint` string.

### Exit Status

| Status | Meaning |
| --- | --- |
| `0` | Success. |
| `2` | Usage error detected during command execution. |
| `4` | Selector resolution or input value error. |
| `5` | Connection, protocol, device, discovery, or runtime parameter error. |
| `6` | Output generation or writing error. |

Clap handles syntax errors before `og-param` selects an output mode. Missing commands, unknown options, invalid slots, and similar early errors use Clap's text output on standard error and normally return status `2`, even if the command line contains `--format json`.

Scripts must check both the process exit status and the JSON `ok` field. Do not treat valid JSON by itself as success.

## CSV Schema

Only `list` supports CSV. The current layout has one header row and one row per selected parameter:

```csv
OID,Display Name,Menu,Type,Access,Role,Constraint,Possible Values
0x0400,Operating Mode,System,int16,read_write,operational,choice,Disabled=0 | Enabled=1
```

| Column | Meaning |
| --- | --- |
| `OID` | Numeric OID formatted as `0xNNNN`, or the string OID as received. |
| `Display Name` | Device-provided display name. |
| `Menu` | Menu paths separated by ` \| `. An ambiguous menu name is formatted as `Menu (Group)`. |
| `Type` | Simplified parameter type name. |
| `Access` | `read_only` or `read_write`. |
| `Role` | `operational` or `layout`. |
| `Constraint` | Constraint category. |
| `Possible Values` | Choice, alarm, or range details separated by ` \| `. |

`Type` is one of `int16`, `int32`, `float32`, `string`, `int16_array`, `int32_array`, `float32_array`, `string_array`, `binary`, or `type_N`, where `N` is an unsupported type ID.

`Constraint` is one of `unconstrained`, `choice`, `string_choice`, `range`, `alarm_table`, `external_resolved`, or `unsupported`.

`Possible Values` uses these forms:

| Constraint | Form |
| --- | --- |
| Numeric choice | `Display Name=raw_value` |
| String choice | The string value |
| Alarm table | `Alarm Name=bit N` |
| Range | `minimum..maximum` or `minimum..maximum step value` |
| Other | Empty field |

The output follows standard CSV quoting rules. Fields that contain commas, quotes, or line endings are quoted and escaped by the CSV writer. Use a CSV parser. Do not split rows on commas.

An unfiltered CSV list includes both operational and layout parameters.

## Script Examples

Read a raw value with [`jq`](https://github.com/jqlang/jq):

```sh
og-param 192.0.2.10 1 read "Operating Mode" --format json \
  | jq -r '.result.value.value'
```

Select writable parameters:

```sh
og-param 192.0.2.10 1 list --format json \
  | jq -r '.result.parameters[] | select(.access == "read_write") | [.oid, .display_name] | @tsv'
```

Check the schema version and success state:

```sh
response=$(og-param 192.0.2.10 1 info --format json) || {
  jq -r '.error.message // "og-param failed without a JSON error"' <<EOF
$response
EOF
  exit 1
}

jq -e '.schema_version == 6 and .ok == true' <<EOF
$response
EOF
```

Process CSV with a CSV-aware tool or library. The ` | ` separators inside `Menu` and `Possible Values` are part of each CSV field, not CSV column delimiters.
