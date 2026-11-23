# Configuration storage

Keep transaction, generation/conflict, integrity, backup and recovery mechanisms in
Rust. Payload bytes are opaque. Hosts own schemas, semantic validation, migration
interpretation, editing, save timing and UI. Never import host models, legacy keys,
commercial policy or platform UI into this package.

Use Git on main and commit coherent, verified changes. Coordinate commits,
publication, artifact builds, CI/service/VM lifecycle and consumer pins at repository
scope. Preserve user data, unsaved edits, original recovery, human QA and sealed releases. No real
customer/security/credential/sharing changes. Keep `make check` and public
GitHub-hosted CI independent of private dependencies. Run distribution validation
with the pinned SDK Tooling checkout. Use login:false and task-scoped tools;
maintained docs/comments are English.

Never seed over an unreadable, missing selected, foreign or newer store. Retain
old generations on restore. Copy raw SQLite families before automatic recovery;
raw recovery captures are not verified logical backups. Read-only observation must
not advance a host writer's baseline. Verify backup identity again on restore after
host validation. C views borrow result owners; copy before freeing.
