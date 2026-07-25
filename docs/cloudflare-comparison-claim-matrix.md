# Toolport vs Cloudflare MCP portals — claim matrix (pre-page)

**Date:** 2026-07-24  
**Purpose:** Every cell of a public comparison page must map to shipped code or an explicit **Roadmap** / **N/A by design**. Update this when product changes; do not invent claims on the page.

**Sources:** gap analysis (`cloudflare-gap-analysis.md`), Linear SOU-339/167/342/171/340/341, teams packaging docs, shipped main on toolport + toolport-teams (post SOU-208 TOTP merge).

---

## Positioning (lead with this, not the table)

| Angle             | Claim (allowed)                                                                                                                                            | Must not claim                                                                         |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| Architecture      | Local-first MCP gateway: shared **server set + policy** sync from Teams; **secrets stay on each member’s machine** (OS keychain).                          | “Keys never leave the org” if you mean cloud vault; “central proxy holds credentials.” |
| Enforcement model | **Cooperative + provable:** org policy authored in Teams, enforced in each member’s local gateway; apply **receipts** for instructions + screening policy. | “Inline cloud enforcement,” “Access-style boundary,” “we block at the network edge.”   |
| vs CF             | Better for **local/stdio MCP**, multi-client desktop agents, secretless control plane.                                                                     | Better at Cloudflare One device posture / any-IdP Access for remote-HTTP-only portals. |
| Spend             | **Tool-call rate limits** (day/month, member/tool axes) fail closed in the gateway; **spend budgets** email on overage (estimate showback).                | Model-layer $ hard block / unified AI Gateway billing.                                 |

---

## Feature matrix (page-ready)

Legend: **Ship** = live on main · **Team+** = paid Team tier · **Free** = free tier · **Roadmap** = do not mark as shipped · **N/A** = not our product lane

| Topic                                                          | Cloudflare MCP portals (public claims) | Toolport (honest)                                                                                                        | Status                         | Evidence / notes                                     |
| -------------------------------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ | ------------------------------ | ---------------------------------------------------- |
| Deploy model                                                   | Remote HTTP MCP, Cloudflare edge       | Local gateway (stdio + HTTP bridge) + optional hosted/self-host Teams control plane                                      | Ship                           | Core product wedge                                   |
| Secrets                                                        | Org/remote pattern via CF platform     | **No org secret vault** — keys local only                                                                                | Ship                           | Packaging honesty; secret-source hints still Batch C |
| Shared server catalog                                          | Portal config                          | Teams config pull/push, version history                                                                                  | Ship                           | Team+ history depth                                  |
| Per-server access (who sees which MCP)                         | Yes (portal)                           | Member + group grants; Free keeps last restrictions after lapse                                                          | Ship / Team+ to edit           | `effective_access`                                   |
| Per-tool allow/deny                                            | Portal tool policy                     | Org `allowedTools` / `disabledTools` → profile `tool_scope`; HTTP + stdio                                                | Ship / Team+                   | SOU-167 (+ #459 HTTP)                                |
| Safety policy (destructive, HITL, quarantine, content defense) | Platform controls                      | Team force flags + local toggles; tighten-only                                                                           | Ship                           | Screening policy                                     |
| Policy proof / coverage                                        | Inline = enforced                      | **Apply receipts** + `reported_at` for instructions + screening policy; dashboard coverage                               | Ship                           | SOU-339 / W5                                         |
| Usage showback                                                 | Analytics                              | Per-member/server usage + est. cost; Free + Team                                                                         | Ship                           | `/usage`                                             |
| Spend budget                                                   | $ limits with block (model)            | Monthly **alert** budget (email once/month)                                                                              | Ship / Team+                   | Not a hard block — say so                            |
| Tool-call rate limits                                          | Identity budgets (beta) / platform     | **Hard block** day/month caps (team/member/group/tool); config-distributed; 80% webhook warn                             | Ship / Team+                   | SOU-340 Batch B                                      |
| Audit trail                                                    | Analytics + Logpush                    | Events CSV + usage CSV; Free 14-day floor; **opt-in per-call export** (tool/ts/duration/ok/argsHash, never args/results) | Ship                           | SOU-171; Free retention SOU-139                      |
| Webhooks / alerts                                              | Platform                               | HMAC-signed, retries, `all`\|`security` filter; Free = 1 security channel                                                | Ship                           | SOU-342                                              |
| Dashboard login                                                | Access (any IdP, MFA, posture)         | GitHub/Google OAuth + magic link + **TOTP 2FA**                                                                          | Ship                           | SOU-341 step 1                                       |
| SSO (SAML/OIDC)                                                | Yes (Access)                           | **Roadmap** (WorkOS, Enterprise)                                                                                         | Roadmap                        | SOU-341 step 2 — table must say Roadmap              |
| SCIM                                                           | Often via Access/IdP                   | **Roadmap** / sales                                                                                                      | Roadmap                        | SOU-341 step 3                                       |
| Device posture                                                 | Yes                                    | **N/A** (not local-first lane)                                                                                           | N/A                            | Don’t compete; don’t imply                           |
| DLP / PII product                                              | Yes (AI Gateway path)                  | Config-export redaction only; PII v1 **Roadmap**                                                                         | Roadmap                        | SOU-346                                              |
| Block on injection                                             | Detection/coming soon                  | Detect + label; **opt-in block Roadmap**                                                                                 | Ship (label) / Roadmap (block) | SOU-345                                              |
| Metrics / Logpush                                              | Yes                                    | In-app Activity + org usage; Prom **Roadmap**                                                                            | Roadmap                        | SOU-347                                              |
| API / service tokens                                           | Platform tokens                        | Team API tokens for **full config pull** (CI/deploy); **not** least-privilege scoped yet                                 | Ship (unscoped)                | SOU-343 — **do not claim scoped tokens**             |
| Self-host control plane                                        | N/A / different                        | Yes (Docker image)                                                                                                       | Ship                           | teams.astro self-host section                        |
| Free tier                                                      | —                                      | Up to 5 people, shared set, safety, 14-day audit                                                                         | Ship                           | Pricing already on site                              |

---

## Site / copy drift to fix **with** the page (not blockers for code)

Current `toolport-site` `/teams` still under-sells post-gap work. When drafting the comparison page (or refreshing `/teams`):

| Current site tendency                             | Update to                                                                  |
| ------------------------------------------------- | -------------------------------------------------------------------------- |
| “Spend budgets” as primary cost control           | Budgets = **alerts**; add **tool-call rate limits** (hard, Team+)          |
| “Exportable audit trail” only as summarized usage | Add **opt-in per-call export** + retention windows                         |
| Governance = access + roles                       | Add **per-tool allowlists**, **policy coverage receipts**                  |
| SSO listed as Enterprise ask only                 | Keep SSO as sales/roadmap; add **2FA available now** on dashboard accounts |
| Alerts unspecified                                | Signed webhooks, security filter, Free security channel                    |

Do **not** change Enterprise SSO/SCIM from “contact us / on your terms” into “shipped.”

---

## Adversarial FAQ (put on page or comparison footer)

1. **“Is this a security boundary like Cloudflare Access?”**  
   No. Policy is authored in Teams and enforced by each member’s local Toolport gateway. A disconnected client is outside org policy. Receipts show who applied what and when.

2. **“Where do API keys live?”**  
   On each member’s machine (OS keychain). The Teams server stores the server _set_ and non-secret policy, not secrets.

3. **“Do you hard-block runaway agents?”**  
   Yes for **tool-call rate limits** (Team+). Monthly spend budgets **alert** admins; they do not block model spend at a cloud LLM meter.

4. **“SSO?”**  
   Dashboard: OAuth + magic link + TOTP. SAML/OIDC/SCIM is Enterprise roadmap (WorkOS), not self-serve today.

5. **“Service accounts / least privilege tokens?”**  
   API tokens can pull full shared config for deploy/CI. Scoped/expiring tokens are tracked (SOU-343), not shipped as a product claim yet.

---

## Housekeeping checklist before page goes live

### Code (SOU-344) — **done on main**

| Item                                                          | State                                                            |
| ------------------------------------------------------------- | ---------------------------------------------------------------- |
| `no_payment_required` (100%-off) success path                 | Fixed #146; `checkout_session_is_paid` + tests                   |
| Custom roles after Free downgrade (resend + redeem/join link) | Fixed #145; `allow_custom_role` + resend tier gate + store tests |

### Page launch gate (process)

- [x] Comparison page draft uses only **Ship** / **Roadmap** / **N/A** from this matrix (`/compare/cloudflare/`)
- [x] Refresh `/teams` marketing bullets for rate limits, receipts, per-tool policy, 2FA, webhooks
- [x] Explicit cooperative-enforcement paragraph (no boundary cosplay)
- [x] SSO/SCIM rows marked Roadmap, not green checkmarks
- [x] SOU-343 language only in FAQ (no scoped-token claim)
- [ ] After page: resume P2 (345–348) as lead-extension, not page deps

### Explicit non-goals for v1 comparison page

- Building WorkOS just to tick SSO
- Claiming Prom/Logpush/PII
- Model-layer $ blocking
- Inline cloud MCP proxy

---

## Suggested page skeleton

1. **Hook:** Shared MCP governance without a central key vault.
2. **How it works (3 steps):** Author in Teams → sync to members → enforce + prove on local gateway.
3. **Matrix** (subset of table above, 8–12 rows max).
4. **Honest differences** (CF wins: Access/IdP/posture/remote-HTTP edge; Toolport wins: local/stdio, secretless control plane, tool-layer caps + receipts).
5. **CTA:** Free team / Team trial / self-host.
6. **Footnotes:** Cooperative model; budget vs rate limit; retention Free vs Team.

---

## Linear cross-links

| Issue       | Relation to page                                                                                    |
| ----------- | --------------------------------------------------------------------------------------------------- |
| SOU-344     | Honesty gate — code edges already fixed; this matrix is the remaining “adversarial reading” control |
| SOU-343     | Post-page hardening unless page claims scoped tokens                                                |
| SOU-341     | Step 1 (2FA) = Ship; step 2–3 = Roadmap rows                                                        |
| SOU-345–348 | Post-page P2; optional “coming next” not required on v1                                             |
