<!--
One PR closes one issue: "Closes #N". Keep the PR scoped to that issue.
Conventional Commit title (feat:/fix:/docs:/chore:/test:/refactor:); body explains why.
-->

## What & why

<!-- The outcome of this PR, in a sentence or two. -->

Closes #N

## How tested

<!-- Commands run and evidence (just lint / just test / integration results). -->

## Checklist

- [ ] Spec/plan exists at `specs/<N>-<slug>/` for non-trivial work, and the plan was approved before implementation
- [ ] `just lint` and `just test` pass
- [ ] New permanent decisions recorded in `docs/adr/` (if any)
- [ ] Whitepaper updated if semantics changed (source of truth)
- [ ] No scope-guardrail violations (whitepaper §3.1 / §23)
