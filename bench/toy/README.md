# toy

A tiny fixture crate for agent-runtime experiments (M0 to M2). Two agents get prompts that both require editing the same function in `src/lib.rs`. Every run copies this crate to a temp directory, so runs never dirty the repo.
