`team-safety-policy.json` contains member `GET /config` responses captured from
Toolport Teams commit `89a1828` (service PR #267) using a synthetic local database.
It covers each floor, independent protections and migrated legacy policy.

When a floor is absent, legacy `denyDestructive: true` migrates to Strict;
otherwise legacy forced approval migrates to Ask, and other policies use Off.
Explicit floors are preserved. The client also derives this floor from old-service
payloads with missing or invalid floors, and releases it on leave or a later sync
without the policy. These old-service cases are covered directly in `teams.rs`.
