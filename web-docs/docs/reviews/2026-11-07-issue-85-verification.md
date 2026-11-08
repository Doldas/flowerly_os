# Issue #85 — Storage-mode parser: verifieringsinkrement

**Status: delimplementation / draft PR, inte accepterad som färdig ticket.**

## Implementerat 2026-11-07

Ytterligare Rust-regressionstester mot CRLF, inledande/avslutande blanksteg och icke-kanoniska modevärden.

## Bevis och testbarhet

Koden och regressionstesten ligger i samma branch som denna rapport. Kontrollera enbart GitHub Actions på PR:ens exakta HEAD SHA; offline-kontroll är inte verkligt Arch/KVM- eller owner-godkännande. Ingen security-critical test avaktiveras, inga databaser eller ägarroller migreras av denna PR.

## Återstående acceptans / operatoråtgärder

Mode-gränssnitt är inte inkopplat i tjänstestart; verkliga migrerings- och restart/boot-gates återstår.

## Merge/överlämning

Merge inte före godkända rust, docs, security, CodeQL och pr-validation på exakt commit, relevant ägargodkännande samt kontroll av diff mot senaste release/2. Kommentera den verkliga merge-SHA på issue vid eventuell merge. Nuvarande issue hålls öppet. Se AGENTS.md och release-0.0.4-completion-plan.md.
