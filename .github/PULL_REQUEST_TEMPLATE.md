## What this PR does

<!-- One paragraph describing the change. -->

## Approach and tradeoffs

<!-- Why this design? What was consciously left out? -->

## How to test locally

```bash
cd <module> && go test ./...   # or: cargo test
```

## Checklist

- [ ] Tests cover every new behaviour (TDD — written before the impl)
- [ ] No private keys or .pem files committed
- [ ] `go vet` / `cargo clippy` passes with no warnings
- [ ] Conventional Commits format used on all commits
- [ ] PR does not target `main` directly
