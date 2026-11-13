## 1. Foundations
- [ ] 1.1 Expiry on `Credentials`; operations refuse to send with expired credentials
- [ ] 1.2 Field reader for JSON and XML responses in a fixed buffer; property tests and a fuzz target
- [ ] 1.3 Generalise rotation from the file provider to a provider interface with a dedicated connection slot and buffer

## 2. Providers
- [ ] 2.1 Environment variables and shared credentials and config files, read at build time
- [ ] 2.2 EC2 instance metadata (IMDSv2)
- [ ] 2.3 ECS and EKS container endpoints, including the authorization token file
- [ ] 2.4 Web identity through STS, rereading the token file on each refresh
- [ ] 2.5 Default chain and region discovery

## 3. Tests
- [ ] 3.1 Simulated metadata, container and STS endpoints in the test server
- [ ] 3.2 Simulation tests: first load, refresh before expiry, refresh failure with backoff, expiry with no replacement, faults in every phase
- [ ] 3.3 Allocation guard stays sealed after build

## 4. Real environments
- [ ] 4.1 Run on an EC2 instance with an instance profile and record the result
- [ ] 4.2 Run in a container with a task or pod role if one is available
- [ ] 4.3 README: credential sources and their order
