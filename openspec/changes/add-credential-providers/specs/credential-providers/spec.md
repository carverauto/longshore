## ADDED Requirements

### Requirement: Credential sources
The client SHALL obtain credentials from environment variables, the shared
credentials and config files, the EC2 instance metadata service using IMDSv2,
the ECS and EKS container credential endpoints, and web identity through STS.

#### Scenario: Instance profile
- **WHEN** the client is built on an EC2 instance with an instance profile and no other credentials are configured
- **THEN** requests are signed with the profile's temporary credentials

#### Scenario: Web identity
- **WHEN** a web identity token file and a role are configured
- **THEN** the client exchanges the token with STS and signs requests with the returned credentials

### Requirement: Default chain
The client SHALL offer a default chain that tries sources in a documented
order and uses the first that yields credentials.

#### Scenario: Environment wins
- **WHEN** credentials are present in the environment and the instance also has a profile
- **THEN** the environment credentials are used and the metadata service is not contacted

#### Scenario: Nothing found
- **WHEN** no source yields credentials
- **THEN** building the client fails with an error that lists the sources tried

### Requirement: Refresh ahead of expiry
Temporary credentials SHALL be refreshed before they expire on a schedule
the client chooses, and new credentials SHALL replace the old ones only after
the store accepts them.

#### Scenario: Refresh during traffic
- **WHEN** credentials near expiry while operations are in progress
- **THEN** new credentials are fetched and adopted and no operation fails because of the change

#### Scenario: Refresh keeps failing
- **WHEN** every refresh attempt fails until the credentials expire
- **THEN** operations submitted after expiry fail with a credentials error and nothing is sent with expired credentials

### Requirement: Providers follow the first principles
Provider requests SHALL go through the `Io` trait, SHALL NOT allocate after
the client is built, and SHALL NOT take connection or operation slots away
from data requests.

#### Scenario: Sealed heap
- **WHEN** a refresh from each network provider runs in the simulator with the allocation guard sealed
- **THEN** no allocation is attempted

#### Scenario: Pool full
- **WHEN** every data connection is busy and a refresh is due
- **THEN** the refresh proceeds on its own connection slot
