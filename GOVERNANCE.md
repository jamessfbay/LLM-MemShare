# Governance

LLM-MemShare is currently maintained by the `jamessfbay` GitHub account. Maintainers review changes, manage releases, respond to security reports, and enforce project policies. Additional maintainers may be invited after sustained, security-conscious contributions.

Routine changes use pull requests, required automated checks, and at least one approving review. Changes to tenancy, authorization, migrations, compatibility, security claims, or release controls require an explicit threat-model and rollback review. Maintainers seek consensus; when consensus is not possible, the repository owner records the decision and rationale in the pull request or issue.

Releases follow [CHANGELOG.md](CHANGELOG.md), the compatibility policy in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md), and the evidence gates in [docs/IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md). A maintainer must not weaken required checks merely to publish a release. Private vulnerability handling follows [.github/SECURITY.md](.github/SECURITY.md).

Governance changes are proposed by pull request and take effect when merged. This lightweight model is appropriate for the current beta; the project will document succession, voting, and broader maintainer roles before a 1.0 release.
