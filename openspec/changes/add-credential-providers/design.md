## Context
`store::s3::rotation` already rereads a credentials file on a timer,
validates the candidate with a one-key listing, and swaps it in. The new
providers differ in where the candidate comes from and in carrying an expiry.

## Goals / Non-Goals
- Goals: run on EC2, ECS, EKS and Lambda without configuration; refresh
  without stalling requests; everything testable in the simulator
- Non-Goals: SSO and other interactive flows, credential process helpers,
  and assume-role chains across profiles. These can follow if asked for.

## Decisions
- One `Provider` state machine interface with a fixed set of implementations
  selected at build time. No trait objects are needed on the data path.
- Provider requests use a dedicated connection slot and a dedicated buffer,
  both sized when the client is built, so a refresh never competes with data
  requests for slots.
- Responses are read with a minimal field reader that finds named string
  fields in a JSON object or XML document inside a fixed buffer. No general
  JSON parser is added. It gets property tests and a fuzz target.
- Refresh starts at a fixed margin before expiry and backs off on failure.
  Current credentials stay in use until they expire. If they expire with no
  replacement, operations fail with a credentials error instead of being
  sent with expired keys.
- IMDSv2 only. The token request is made with a hop limit the service
  accepts, and IMDSv1 is not attempted.
- STS for web identity is an unsigned HTTPS request. Its endpoint is regional
  and resolved like any other host.

## Risks / Trade-offs
- Profile file parsing covers the common keys only. Unknown keys are ignored
  and reported once at build time.
- The instance metadata address is link-local and plain HTTP. The provider
  refuses to send it through a proxy if proxy support is added later.

## Open Questions
- Whether to add an assume-role provider (signed STS call) in this change or
  a later one. Proposed: later, since web identity covers Kubernetes and the
  instance and container providers cover the rest.
- Which instance metadata address the provider uses on an IPv6-only
  instance, where 169.254.169.254 is not reachable and the service answers
  at fd00:ec2::254 only when the instance owner enables it. Proposed,
  pending an owner decision: follow the AWS SDK convention. The IPv4 address
  is the default, and the IPv6 address fd00:ec2::254 is used only when the
  endpoint mode is set to IPv6 in the environment or config file. There is
  no automatic fallback from one to the other.
