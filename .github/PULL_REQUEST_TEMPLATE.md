## Summary

<!-- What does this change do and why? -->

## Related issues / RFCs

<!-- Link to issues, ADRs, or RFCs this change implements or resolves. -->

## Checklist

- [ ] I read the relevant ADRs in `adr/` and the language policy in `docs/LANGUAGE_POLICY.md`
- [ ] Code follows repository conventions and contains no undocumented `unsafe`
- [ ] New durable data has a SQLite migration (not manual schema mutation)
- [ ] Services log important IDs (`flow_id`, `goal_id`, `session_id`, `generation_id`, ...) in structured fields
- [ ] `just fmt-check`, `just lint`, `just test` pass locally
- [ ] `CHANGELOG.md` updated for user-visible changes (when applicable)
- [ ] Commit message uses a conventional prefix (`feat:`, `fix:`, `docs:`, ...)