# Contributing to EH3D-SERVER

Thank you for looking. This server can start a real machine, so changes are small and carefully tested.

1. **Nothing moves a real machine without proof.** A change in the way a job reaches a machine needs a test against the simulator and, when it touches the real link, a note of what was observed on hardware.
2. **Every behaviour has a test**, including the refusals: a job outside the table, a role that may not do something, a machine that is busy, a damaged data file.
3. **The planning is not written here.** It is the library of EH3D-DRILLPRO and EH3D-ROUTEPRO; a change to how a job is planned goes there, so every client sees the same job.
4. **Before a pull request:** `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `python -I tools/ci_validate.py` must pass. The repository builds next to its sibling repositories (see the build section of the README).
5. **Texts exist in eight languages** (English, Spanish, French, Italian, German, Chinese, Japanese and European Portuguese) wherever a person reads them. The README files keep the same structure.
6. **No secrets in the repository**: tokens, passwords and node tokens live in the server's data folder, which is never committed.
7. Commits are written in English, small and about one thing.
