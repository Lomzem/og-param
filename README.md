# og-param

Discovers, reads, and writes openGear parameters over binary OGP. Parameter OIDs, descriptors, constraints, and choice labels are queried from the connected device at runtime, so one executable works with different cards and firmware versions.

## CLI

Every command starts with the frame controller host and card slot:

```text
og-param <host> <slot> info [--format json] [--no-force]
og-param <host> <slot> list [<parameter>] [--menu <menu>] [--group <group>] [--format table|json|csv] [--legacy-schema] [--no-force]
og-param <host> <slot> read <parameter> [--menu <menu>] [--group <group>] [--format json] [--no-force]
og-param <host> <slot> write <parameter> [<value>] [--menu <menu>] [--group <group>] [--format json] [--no-force]
```

Options may appear before, between, or after positional arguments.

### Inspect

```sh
# Device identity and discovered parameter counts
og-param 10.3.112.50 14 info

# All operational parameters, their menus, and possible values
og-param 10.3.112.50 14 list

# One globally unique parameter
og-param 10.3.112.50 14 list "System Genlock"

# All parameters in a menu, or one parameter in that menu
og-param 10.3.112.50 14 list --menu "SDI In 1"
og-param 10.3.112.50 14 list "Colorspace" --menu "SDI In 1"

# Machine-readable output
og-param 10.3.112.50 14 list --format json
og-param 10.3.112.50 14 list --format csv
og-param 10.3.112.50 14 list --format csv --legacy-schema
```

### Read

Globally unique display names need no menu:

```sh
$ og-param 10.3.112.50 14 read "Die Temperature"

0x020D (Die Temperature): 44
```

Use `--menu` when a display name is ambiguous:

```sh
og-param 10.3.112.50 14 read "Colorspace" --menu "SDI In 1"
```

If the menu name is also ambiguous, add its group:

```sh
og-param 10.3.112.50 14 read "Card Name" --menu "Card" --group "CONFIGURATION"
```

Numeric and semantic string OIDs work directly:

```sh
og-param 10.3.112.50 14 read 0x0105
og-param 10.3.112.50 14 read oid:0x0105
og-param 10.3.112.50 14 read oid:mle.2.keyer.3.ckey-state
```

### Write

Write a raw value or a displayed choice label:

```sh
$ og-param 10.3.112.50 14 write "System Genlock" 4
0x0400 (System Genlock): Free Run (raw_value=4)

$ og-param 10.3.112.50 14 write "System Genlock" "Free Run"
0x0400 (System Genlock): Free Run (raw_value=4)
```

> Successful writes print the response from the Card.

You could see the list of possible values with the `list` command:

```sh
$ og-param 10.3.112.50 14 list "System Genlock"
OID     DISPLAY NAME    MENU    TYPE   ACCESS
0x0400  System Genlock  System  int16  read_write
  Possible values:
    +----------------+-----------+
    | VALUE          | RAW VALUE |
    +----------------+-----------+
    | Auto           | 0         |
    | SDI In 1       | 1         |
    | SDI In 2       | 5         |
    | HDMI In        | 2         |
    | OG Frame Ref 1 | 3         |
    | OG Frame Ref 2 | 6         |
    | Free Run       | 4         |
    +----------------+-----------+
```

### Selectors

Display names, menu names, group names, and choice labels are **not case sensitive**.

```sh
# Both valid
og-param 10.3.112.50 14 read "Colorspace" --menu "SDI In 1"
og-param 10.3.112.50 14 read "colorspace" --menu "sdi in 1"
```

Automatic selectors accept OIDs and display names. If an OID is also another parameter's display name, use `oid:<value>` or `name:<value>` to state the intended interpretation explicitly.
Explicit `oid:<value>` reads and writes fetch only that parameter's descriptor, so they do not depend on unrelated catalog or menu metadata.

```sh
# Select OID 0x0105
og-param 10.3.112.50 14 read oid:0x0105

# Select the parameter whose display name is "0x0105"
og-param 10.3.112.50 14 read name:0x0105
```

Ambiguous names are never guessed. The error lists usable `--menu` qualifiers and OIDs.

If multiple parameters have both the same display name and the same menu, no menu qualifier can distinguish them. The error lists each candidate's OID; select the intended parameter explicitly:

```sh
og-param 10.3.112.50 14 read oid:0x1201
```

To determine which OID belongs to a parameter in DashBoard, enable `Parameter Inspector Mode` and click the control to view its debug information. Use the reported OID with `oid:<OID>` for subsequent `list`, `read`, and `write` commands.

You could enable `Parameter Inspector Mode` by clicking `Views > openGear Parameter Inspector`.

### Connection and Formats

Connections are forced by default for the manufacturing workflow. Use `--no-force` to avoid displacing another client.

`info`, `read`, and `write` support human or JSON output. `list` supports table, JSON, current CSV, and legacy CSV. The legacy CSV's `Parameter Name` column contains the OID.

The runtime reads and writes scalar integers, floats, strings, choices, ranges, and alarms. Arrays, binary values, and unknown types are list-only. Authentication is not supported.

### Scripting

`og-param` is built to work well with scripts.

All command outputs could be formatted into structured JSON output by including `--format json`:

```sh
$ og-param 10.3.251.15 15 write "SFP 1 Mode" "Receive" --format json
{
  "ok": true,
  "operation": "write",
  "result": {
    "display_value": {
      "kind": "label",
      "value": "Receive"
    },
    "parameter": {
      "display_name": "SFP 1 Mode",
      "oid": "0x0503"
    },
    "requested_value": {
      "kind": "int16",
      "value": 0
    },
    "value": {
      "kind": "int16",
      "value": 0
    }
  },
  "schema_version": 5
}

$ og-param 10.3.251.15 15 read "SFP 1 Mode" --format json
{
  "ok": true,
  "operation": "read",
  "result": {
    "display_value": {
      "kind": "label",
      "value": "Receive"
    },
    "parameter": {
      "display_name": "SFP 1 Mode",
      "oid": "0x0503"
    },
    "value": {
      "kind": "int16",
      "value": 0
    }
  },
  "schema_version": 5
}
```

## Build

Build locally with Rust:

```sh
cargo build --release --locked
```

Docker can produce static Linux and Windows executables without requiring a firmware checkout:

```sh
scripts/build-in-docker.sh output
```

On Windows:

```powershell
scripts/build-in-docker.ps1 output
```

This produces:

- `og-param-linux-x86_64`
- `og-param-windows-x86_64.exe`

Pass a second argument to choose a different output basename. Set `OG_PARAM_SKIP_DOCKER_BUILD=1` to reuse the existing image or `OG_PARAM_ARTIFACT_IMAGE` to choose another image tag.


## Development

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
```
