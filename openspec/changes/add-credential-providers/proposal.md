## Why
Credentials come from static keys, a session token, a bearer token, or a file
that is reread. On AWS most workloads get credentials from the instance or
container they run on, or by exchanging a web identity token, and expect a
client to find them without configuration.

## What Changes
- Providers for environment variables, the shared credentials and config
  files, the EC2 instance metadata service (IMDSv2), the ECS and EKS container
  endpoints, and web identity through STS
- A default chain that tries them in the usual order
- Refresh ahead of expiry on the client's own schedule, with the new
  credentials adopted only after the store accepts them, as file rotation
  works today
- Region discovery from the environment, config file or instance metadata

## Impact
- Affected specs: `credential-providers` (new)
- Affected code: `store::creds` (expiry, provider state machines),
  `store::s3::rotation` (generalised from the file provider), a small JSON
  and XML field reader, `http` client use for the metadata and STS endpoints
- First principles: provider requests go through the same `Io` trait and
  connection pool, parse into fixed buffers, and run in the simulator.
  Environment variables and profile files are read when the client is built,
  where allocation is allowed.
