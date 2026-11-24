# OpenSpec adapter

NEMESIS uses OpenSpec's official artifact and schema surfaces as a read-only integration:

- detects `openspec/` and `openspec/config.yaml`;
- resolves the configured schema or the built-in `spec-driven` dependency shape;
- reports active changes and artifact paths;
- never overwrites `openspec/schemas/` or OpenSpec-managed skills/commands.

The implemented installer records this adapter under `.nemesis/integrations.json`. It intentionally has no managed upstream files because OpenSpec's documented extension surface is custom schemas plus generated agent delivery, not a general third-party adapter registry.
