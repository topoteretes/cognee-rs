# T14 — Maven Central publishing wiring  (Blocked: infra)

> **Status: blocked on infrastructure.** This task authors the publishing job
> but it cannot be completed until the infra prerequisites below exist
> (design §9 decisions #4 and #8). Do **not** un-block or "complete" this task by
> assuming credentials/namespace are in place. The publish job must be authored
> so it **silently skips** when the secret is absent (mirroring
> `ts-prebuild.yml`'s `check-token` gate), so merging it changes nothing until
> infra is ready.

## Infra prerequisites (NOT code — cannot be done in this repo)

1. **Sonatype Central / OSSRH namespace ownership for `ai.cognee`** (decision
   #4) — verified with the domain owner. Until confirmed, the group id is
   provisional.
2. **Publishing credentials + GPG signing key** (decision #8) — added as repo
   secrets (e.g. `MAVEN_CENTRAL_USERNAME`, `MAVEN_CENTRAL_PASSWORD`,
   `MAVEN_GPG_PRIVATE_KEY`, `MAVEN_GPG_PASSPHRASE`).

## Dependencies & preconditions

- **T13 done** (classifier jars build on the matrix; `java-prebuild.yml` exists).
- Read `.github/workflows/ts-prebuild.yml`'s `check-token` + `publish` jobs (the
  token-gate pattern: `secrets` cannot be used in a job-level `if:`, so a
  precheck job outputs a boolean the publish job gates on) and the release
  patterns in `release-publish.yml` if present.

## Context for this task

Maven Central requires, per artifact: the main jar (classes), a `-sources` jar,
a `-javadoc` jar, a POM with license/scm/developer metadata, GPG signatures for
all of them, and (for this binding) the per-classifier native jars from T13.
Consumers select the native jar via the os-detector plugin or an explicit
`<classifier>`.

## Steps

### 1. Extend `java/pom.xml` with Central-publishing metadata + plugins

Add (guarded behind a `release` profile so normal `mvn verify` is unaffected):

- `<licenses>` (Apache-2.0 / MIT dual, matching the workspace), `<scm>`,
  `<developers>`, `<url>` — Central's required POM metadata.
- `maven-source-plugin` (attach `-sources`), `maven-javadoc-plugin` (attach
  `-javadoc`; already configured in T12 — ensure it attaches in the release
  profile), `maven-gpg-plugin` (sign), and the Central publishing plugin
  (`central-publishing-maven-plugin` for the Central Portal, or
  `nexus-staging-maven-plugin` for legacy OSSRH — pick per the namespace's
  onboarding).

### 2. Extend `.github/workflows/java-prebuild.yml` with a gated publish job

Add a `check-token` precheck job (clone from `ts-prebuild.yml`) that outputs
`has_token` from the presence of `MAVEN_CENTRAL_PASSWORD`, and a `publish` job:

```yaml
  publish:
    name: Publish to Maven Central
    needs: [build-platform, check-token]
    runs-on: ubuntu-latest
    # `always()` so it runs even if some matrix legs failed; skips silently
    # when the secret is absent (pre-infra). Mirrors ts-prebuild's publish gate.
    if: ${{ always() && needs.check-token.outputs.has_token == 'true' }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-java@v4
        with:
          distribution: temurin
          java-version: "17"
          # server-id + gpg config for Central deploy
      - name: Download classifier jars
        uses: actions/download-artifact@v4
        with:
          pattern: java-*
          path: /tmp/java-artifacts
      # Deploy: main jar + sources + javadoc + the 4 classifier native jars,
      # all GPG-signed, via the release profile.
      - name: Deploy
        env:
          MAVEN_CENTRAL_USERNAME: ${{ secrets.MAVEN_CENTRAL_USERNAME }}
          MAVEN_CENTRAL_PASSWORD: ${{ secrets.MAVEN_CENTRAL_PASSWORD }}
          MAVEN_GPG_PRIVATE_KEY: ${{ secrets.MAVEN_GPG_PRIVATE_KEY }}
          MAVEN_GPG_PASSPHRASE: ${{ secrets.MAVEN_GPG_PASSPHRASE }}
        run: |
          # mvn -P release deploy, attaching each classifier jar via
          # build-helper-maven-plugin attach-artifact or deploy:deploy-file.
          echo "publish wiring — see task T14; requires infra secrets"
```

### 3. Document the consumer story in `java/README.md`

Once published, consumers add `ai.cognee:cognee` plus a classifier (or the
os-detector plugin). Add that snippet to `java/README.md` under a "Coming to
Maven Central" note (kept accurate: not yet published).

## Verification

- **Cannot be verified without infra.** When the secrets/namespace exist, a
  `workflow_dispatch` run of `java-prebuild.yml` deploys to the Central staging
  repository and the artifacts appear (validate the staging bundle passes
  Central's checks: signatures present, POM metadata complete, sources+javadoc
  jars attached).
- Until then: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/java-prebuild.yml'))"`
  lints clean, and `mvn -q -P release -DskipTests -Dgpg.skip=true -f java/pom.xml package`
  builds the main + sources + javadoc jars locally (signing skipped).

## Out of scope

- Actually publishing (requires the human "go" + infra).
- Automating version bumps / release notes (follows the existing release
  tooling; not a Java-specific concern).
