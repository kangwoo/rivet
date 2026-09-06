//! Tool dispatch: validate -> intercept -> policy -> approve -> sandbox -> execute.
//!
//! Implemented in Phase 1 (validate/execute) and Phase 4 (policy/sandbox). The ordering
//! is fixed by `docs/architecture.md` and must not be reordered by a host: schema
//! validation before policy means a policy always sees well-formed input.
