# dui

`dui` is a container-first Docker TUI for local development.

[日本語版 README](./README.ja.md)

It keeps the main screen focused on containers instead of splitting attention across images, networks, and volumes. The first release emphasizes:

- A fast container list with Compose project and service context
- Strong single-container log viewing with search, substring or regex filters, follow mode, wrapping, and clipboard copy
- Structured inspect views for overview, env vars, ports, mounts, and health
- Keyboard-first lifecycle actions: start or stop, restart, and remove with confirmation

## Install

Install the latest GitHub Release on Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/dui/main/install.sh | sh
```

Install to a custom directory:

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/dui/main/install.sh | sh -s -- -b=$HOME/.local/bin
```

Install a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/dui/main/install.sh | sh -s -- -v=v0.0.2
```

Install from GitHub with Cargo:

```bash
cargo install --git https://github.com/igtm/dui.git --locked
```

Install from a local checkout:

```bash
cargo install --path . --locked
```

## Run

```bash
cargo run
```

Stopped and exited containers are visible by default. Press `a` to hide or show stopped containers, or set `show_stopped_by_default = false` in your config to start with only active containers.

Optional startup flags:

```bash
cargo run -- --project demo --container api --theme ember
```

You can also build and run the binary directly:

```bash
cargo build
target/debug/dui
```

## Keybindings

- `q`: quit
- `Tab`: switch focus between container list and detail pane
- `1-6`: switch detail tabs
- `a`: hide or show stopped containers
- `y`: copy the current selection
- `s`: start or stop selected container
- `r`: restart selected container
- `D`: remove selected container with confirmation
- `/`: search logs
- `f`: filter logs
- `m`: toggle log filter mode between substring and regex
- Mouse wheel: move the hovered selection by one row
- Left drag in logs: select a contiguous log range
- Drag the right scrollbar: move the visible window
- `Space`: toggle log follow mode
- `w`: toggle wrapping in logs
- `t`: toggle timestamps in logs

## Config

Default config path:

- Linux: `$XDG_CONFIG_HOME/dui/config.toml`
- macOS: `~/Library/Application Support/dui/config.toml`

See [`examples/config.toml`](./examples/config.toml) for a sample config.

## Status

`dui` currently targets local Docker Engine workflows on Linux and macOS. It is intentionally narrow: containers and container logs are the center of the UI. Images, networks, and volumes are out of scope.
