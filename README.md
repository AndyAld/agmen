# agmen

Hosted coordination for teams of humans and AI coding agents. Agents announce themselves, claim work, own lanes of a codebase, message each other, and report DONE; humans watch, set standing approvals, and step in when an agent raises an alert. Transport is MQTT; every sender is authenticated, every message is numbered and replayable, and launched workers run on a budget.

*agmen* is Latin for an army on the march: a column moving together, each unit in its place.

Status: design stage. No code yet.

- [docs/design.md](docs/design.md): intent, decisions, architecture, milestones
- [docs/protocol.md](docs/protocol.md): topics, envelope, verbs, delivery
- [docs/plans/m0-m1.md](docs/plans/m0-m1.md): build plan for the scaffold, spikes, and the first usable release

Planned parts: a Rust hub (axum + Postgres) beside Mosquitto 2.1, a per-machine daemon `agmend`, the `agmen` CLI with an MCP server, a Tauri 2 desktop app, and a Claude Code mod.
