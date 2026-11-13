// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Credentials in fixed storage, and a parser for the shared credentials
//! file format (`aws_access_key_id = ...` under `[default]`).

pub const FIELD_MAX: usize = 128;
/// Longest bearer token. Google's access tokens are documented as up to 2048 bytes.
pub const TOKEN_MAX: usize = 2048;
/// Longest session token that accompanies temporary keys.
pub const SESSION_MAX: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Credentials {
    access: [u8; FIELD_MAX],
    access_len: usize,
    /// The secret key, or a bearer token when there is no access key.
    secret: [u8; TOKEN_MAX],
    secret_len: usize,
    /// The session token of temporary keys; empty for long-lived ones.
    session: [u8; SESSION_MAX],
    session_len: usize,
}

impl core::fmt::Debug for Credentials {
    /// Never prints the secret.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Credentials {{ access_key: {:?}, secret_key: <{} bytes> }}",
            self.access_key(),
            self.secret_len
        )
    }
}

impl Credentials {
    /// `None` if either part is empty, too long, or not printable ASCII.
    pub fn new(access_key: &str, secret_key: &str) -> Option<Credentials> {
        let ok = |s: &str| {
            !s.is_empty() && s.len() <= FIELD_MAX && s.bytes().all(|b| b.is_ascii_graphic())
        };
        if !ok(access_key) || !ok(secret_key) {
            return None;
        }
        let mut c = Credentials {
            access: [0; FIELD_MAX],
            access_len: access_key.len(),
            secret: [0; TOKEN_MAX],
            secret_len: secret_key.len(),
            session: [0; SESSION_MAX],
            session_len: 0,
        };
        c.access[..access_key.len()].copy_from_slice(access_key.as_bytes());
        c.secret[..secret_key.len()].copy_from_slice(secret_key.as_bytes());
        Some(c)
    }

    /// Temporary keys, as a role or a token service hands out: the two keys
    /// and the session token that must travel with them. `None` if any part
    /// is empty, too long, or not printable ASCII.
    pub fn with_session(
        access_key: &str,
        secret_key: &str,
        session_token: &str,
    ) -> Option<Credentials> {
        let mut c = Credentials::new(access_key, secret_key)?;
        if session_token.is_empty()
            || session_token.len() > SESSION_MAX
            || !session_token.bytes().all(|b| b.is_ascii_graphic())
        {
            return None;
        }
        c.session[..session_token.len()].copy_from_slice(session_token.as_bytes());
        c.session_len = session_token.len();
        Some(c)
    }

    /// Empty unless the keys are temporary.
    pub fn session_token(&self) -> &str {
        core::str::from_utf8(&self.session[..self.session_len]).expect("validated as ASCII")
    }

    /// A bearer token, sent as it is instead of a signature. `None` if it is
    /// empty, longer than [`TOKEN_MAX`], or not printable ASCII.
    pub fn bearer(token: &str) -> Option<Credentials> {
        if token.is_empty()
            || token.len() > TOKEN_MAX
            || !token.bytes().all(|b| b.is_ascii_graphic())
        {
            return None;
        }
        let mut c = Credentials {
            access: [0; FIELD_MAX],
            access_len: 0,
            secret: [0; TOKEN_MAX],
            secret_len: token.len(),
            session: [0; SESSION_MAX],
            session_len: 0,
        };
        c.secret[..token.len()].copy_from_slice(token.as_bytes());
        Some(c)
    }

    /// Parse a file that holds one bearer token and nothing else, apart from
    /// surrounding whitespace: what a token helper writes.
    pub fn parse_bearer(text: &[u8]) -> Option<Credentials> {
        Credentials::bearer(core::str::from_utf8(text).ok()?.trim())
    }

    /// Empty for a bearer token.
    pub fn access_key(&self) -> &str {
        core::str::from_utf8(&self.access[..self.access_len]).expect("validated as ASCII")
    }

    pub fn secret_key(&self) -> &str {
        core::str::from_utf8(&self.secret[..self.secret_len]).expect("validated as ASCII")
    }

    /// Parse a credentials file. Keys are read from the `[default]` section,
    /// or from the top of a file that has no sections. Later duplicates win,
    /// as they do in the tools that write these files. A file with an
    /// `aws_session_token` that is unusable is refused whole, never read as
    /// long-lived keys without it.
    pub fn parse(text: &[u8]) -> Option<Credentials> {
        let text = core::str::from_utf8(text).ok()?;
        let (mut access, mut secret, mut session) = (None, None, None);
        let mut in_scope = true;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            // "[name]", optionally followed by a comment.
            if let Some((section, rest)) = line.strip_prefix('[').and_then(|l| l.split_once(']')) {
                let rest = rest.trim();
                if rest.is_empty() || rest.starts_with('#') || rest.starts_with(';') {
                    in_scope = section.trim() == "default";
                    continue;
                }
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            if !in_scope {
                continue;
            }
            match name.trim() {
                "aws_access_key_id" => access = Some(value.trim()),
                "aws_secret_access_key" => secret = Some(value.trim()),
                "aws_session_token" => session = Some(value.trim()),
                _ => {}
            }
        }
        match session {
            Some(token) => Credentials::with_session(access?, secret?, token),
            None => Credentials::new(access?, secret?),
        }
    }
}

#[cfg(test)]
mod tests;
