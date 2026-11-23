# Runs against Amazon S3 that close two changes

Two open changes each wait on one run against Amazon S3. Both runs are
done by `amazon-s3-runs.sh` next to this note, in a few minutes, with one
bucket.

| Change | Task | What the run shows |
|---|---|---|
| `add-reported-operations` | 4.5 Run against Amazon S3 | What S3 answers to `put`, `head` and `get` with conditions, ranges, SSE-S3, SHA-256 and CRC-64, a write whose SHA-256 is of other bytes, and a deadline that has passed (`exact_s3`) |
| `add-ipv6` | 3.3 Run against Amazon S3's dual-stack endpoint from an instance with an IPv6 address | The store over IPv6 through `s3.dualstack.<region>.amazonaws.com` (`s3_uring selftest` with `S3_FAMILY=6`, `prefer6` and `4`) |

The script also runs `s3_uring badcrc` over HTTPS. That settles an open
assumption: the client takes a 400 with `BadDigest` to mean the body was
damaged on the way, and retries. Google Cloud Storage uses `CrcMismatch`
and RustFS uses `BadDigest` (`bench/results/m27-self-hosted-stores.md`),
but Amazon S3's code has not been seen.

`s3_uring badcrc` prints only how the client classed the answer: a
`Fatal(RetriesExhausted)` after 4 attempts means S3 answered `BadDigest`, and a
`Fatal(Status(400))` after 1 attempt means some other code. Which code it
was comes from `badcrc-curl.txt`: the script sends one signed PUT with a
wrong `x-amz-checksum-crc64nvme` straight to S3 with curl, to a key under
`longshore-badcrc/`, and keeps the status line and the body, where the code
is.

## Why not a local store

`bench/results/m27-self-hosted-stores.md` ran the same programs against
Ceph and RustFS in containers. That closed `add-high-level-client` 1.8, but
it cannot close these two: what is being recorded is what Amazon does.
Ceph and RustFS already disagree on whether a wrong checksum is refused.

## What is needed

- A Linux host on kernel 6.1 or later with a Rust toolchain, curl 7.75 or
  later (for `--aws-sigv4`) and this repository, with `RLIMIT_MEMLOCK`
  large enough for the buffers (the benchmark instances already are).
- An S3 bucket in a known region, with a name without dots.
- Keys that can put, get, head and delete objects in that bucket, and
  nothing else. Temporary keys are fine: the script reads
  `AWS_SESSION_TOKEN`.
- For the dual-stack part, a global IPv6 address on the host. On EC2 that
  means a subnet with an IPv6 range and an instance given an IPv6 address.
  Any other host with IPv6 works too, since the endpoint is public. Without
  an address the script skips that part and says so.

## Running it

From the repository root:

```sh
export S3_BUCKET=my-bucket S3_REGION=us-east-1
eval "$(aws configure export-credentials --format env)"   # or export S3_ACCESS_KEY, S3_SECRET_KEY
openspec/notes/amazon-s3-runs.sh
```

It builds `s3_uring` and `exact_s3`, prints the host, commit and IPv6
addresses, and writes each program's output to its own file in
`amazon-s3-runs-<time>/`. A program that fails does not stop the rest.
Nothing is left in the bucket.

## Afterwards

1. Send back the whole output directory.
2. Record it in `bench/results/` like `m24-put-head-get-linode.md` and
   `m19-ipv6-linode.md`: conditions, a table of what the store answered,
   what it shows and what it does not.
3. Tick `add-reported-operations` 4.5 and `add-ipv6` 3.3 with the file
   name, and archive both changes.
4. If S3's code for a checksum mismatch in `badcrc-curl.txt` is not
   `BadDigest`, change it in `crates/longshore_s3/src/s3/outcome.rs`.
   Either way, the comment there that calls it assumed is updated.
