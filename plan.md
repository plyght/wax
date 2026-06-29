1. **Implement `as_str` on `CaskArtifact` in `src/api.rs`**: To avoid hardcoding long mapping of `CaskArtifact` enum variants to strings inside the command function, we'll create an `as_str` method on `CaskArtifact`.
2. **Refactor `info_cask` in `src/commands/info.rs`**: Replace the overly complex mapping logic inside `info_cask` with `.map(|a| a.as_str())`. We'll also simplify the artifacts loop to iterate directly.
3. **Verify Refactor**: Run `cargo check` and `cargo test` to ensure functionality remains the same and build passes.
4. **Complete Pre-commit Steps**: Ensure `pre_commit_instructions` are followed (e.g., proper formatting, linting).
5. **Submit**: Create PR.
