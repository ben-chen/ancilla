# Runtime Programs and Config

Ancilla is now intentionally split into two runtime programs.

## Programs

### `ancilla-server`

Responsibilities:

- host the HTTP API
- own persistence and retrieval logic
- talk to Postgres, AWS, and Bedrock
- own the curated chat model catalog and default model
- provide local admin commands like `capture`, `timeline`, `review`, and `search`

Config file:

- `~/.config/ancilla-server/config.toml`

This program is the thing you deploy to ECS.

### `ancilla-client`

Responsibilities:

- render the ratatui terminal UI
- talk to a running server over HTTP
- fetch the server-advertised model catalog and let the user pick from it
- never read the database directly
- never require AWS credentials or Bedrock settings

Config file:

- `~/.config/ancilla-client/config.toml`

This program is local-only and points at either a local server or the deployed ECS task IP.

## Config Boundary

The split is intentional:

- server config owns storage, AWS, Bedrock, and embedding/runtime knobs
- server config also owns which chat models are available
- client config owns only the remote server address

That keeps deploy concerns out of the TUI and keeps UI concerns out of the server container.

The old unified `~/.config/ancilla/ancilla.toml` is legacy and should not be extended further.

## Operational Consequences

- redeploying the server does not mutate client config
- the deploy script prints the live task IP and leaves it to the operator to update `ancilla-client` if needed
- ECS only needs the `ancilla-server` binary in the container image
- local TUI testing only needs `base_url`

## Recommended Local Setup

1. Create the server config.
2. Create the client config.
3. Start `ancilla-server serve --bind 127.0.0.1:3000`.
4. Run `ancilla-client`.

For deployed testing:

1. Redeploy the server.
2. Get the current task IP.
3. Set `base_url = "http://<ip>:3000"` in `~/.config/ancilla-client/config.toml`.
4. Run `ancilla-client`.
