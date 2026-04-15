# Runtime Programs and Config

Ancilla is now intentionally split into two runtime programs.

## Programs

### `ancilla-server`

Responsibilities:

- host the HTTP API
- own persistence and retrieval logic
- talk to Postgres, AWS, and Bedrock
- optionally call a separate embedder service for live query and memory embeddings
- own the curated chat model catalog and default model
- provide local admin commands like `capture`, `remember`, `timeline`, `review`, and `search`

Config file:

- `~/.config/ancilla/server.toml`

This program is the thing you deploy to ECS.

### `ancilla-client`

Responsibilities:

- render the ratatui terminal UI
- talk to a running server over HTTP
- optionally send HTTP Basic auth from client config
- browse durable memories by default, with a switchable raw-entry timeline
- preview retrieval/context assembly without invoking the chat model
- stream chat answers from the server into the response pane
- fetch the server-advertised model catalog and let the user pick from it
- never read the database directly
- never require AWS credentials or Bedrock settings

Config file:

- `~/.config/ancilla/client.toml`

This program is local-only and points at either a local server or the deployed ECS task IP.

## Config Boundary

The split is intentional:

- server config owns storage, AWS, Bedrock, and embedding/runtime knobs
- server config includes the optional `embedder_base_url` used for synchronous embeddings
- server config also owns which chat models are available
- client config owns the remote server address and optional HTTP Basic auth credentials

That keeps deploy concerns out of the TUI and keeps UI concerns out of the server container.

The old unified `~/.config/ancilla/ancilla.toml` is legacy and should not be extended further. The loaders also still fall back to `~/.config/ancilla-server/config.toml` and `~/.config/ancilla-client/config.toml` if the new shared-dir files do not exist yet.

## Operational Consequences

- redeploying the server does not mutate client config
- the deploy script prints the live task IP and leaves it to the operator to update `ancilla-client` if needed
- ECS only needs the `ancilla-server` binary in the container image
- the optional `ancilla-embedder` runtime is a separate service with its own image
- local TUI testing only needs `base_url` unless the server has Basic auth enabled
- the TUI now treats the memory browser as the primary view and keeps the timeline as a secondary provenance view
- chat streaming is additive; `/v1/chat/respond` stays available while the TUI uses `/v1/chat/respond/stream`

## Recommended Local Setup

1. Create the server config.
2. Create the client config.
3. Start `ancilla-server serve --bind 127.0.0.1:3000`.
4. Run `ancilla-client`.

For deployed testing:

1. Redeploy the server.
2. Set `base_url = "https://ancillabot.com"` or the current ALB DNS name in `~/.config/ancilla/client.toml`.
3. If the server has Basic auth enabled, also set `basic_auth_username` and `basic_auth_password` in `~/.config/ancilla/client.toml`.
4. Run `ancilla-client`.
