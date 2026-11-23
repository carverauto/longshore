// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Translates OS `errno` values into negated Linux errno values (`-EIO` fallback).

pub mod linux {
    pub const EPERM: i32 = 1;
    pub const ENOENT: i32 = 2;
    pub const ESRCH: i32 = 3;
    pub const EINTR: i32 = 4;
    pub const EIO: i32 = 5;
    pub const ENXIO: i32 = 6;
    pub const E2BIG: i32 = 7;
    pub const ENOEXEC: i32 = 8;
    pub const EBADF: i32 = 9;
    pub const ECHILD: i32 = 10;
    pub const EAGAIN: i32 = 11;
    pub const ENOMEM: i32 = 12;
    pub const EACCES: i32 = 13;
    pub const EFAULT: i32 = 14;
    pub const ENOTBLK: i32 = 15;
    pub const EBUSY: i32 = 16;
    pub const EEXIST: i32 = 17;
    pub const EXDEV: i32 = 18;
    pub const ENODEV: i32 = 19;
    pub const ENOTDIR: i32 = 20;
    pub const EISDIR: i32 = 21;
    pub const EINVAL: i32 = 22;
    pub const ENFILE: i32 = 23;
    pub const EMFILE: i32 = 24;
    pub const ENOTTY: i32 = 25;
    pub const ETXTBSY: i32 = 26;
    pub const EFBIG: i32 = 27;
    pub const ENOSPC: i32 = 28;
    pub const ESPIPE: i32 = 29;
    pub const EROFS: i32 = 30;
    pub const EMLINK: i32 = 31;
    pub const EPIPE: i32 = 32;
    pub const EDOM: i32 = 33;
    pub const ERANGE: i32 = 34;
    pub const EDEADLK: i32 = 35;
    pub const ENAMETOOLONG: i32 = 36;
    pub const ENOLCK: i32 = 37;
    pub const ENOSYS: i32 = 38;
    pub const ENOTEMPTY: i32 = 39;
    pub const ELOOP: i32 = 40;
    pub const ENOMSG: i32 = 42;
    pub const EIDRM: i32 = 43;
    pub const ENOSTR: i32 = 60;
    pub const ENODATA: i32 = 61;
    pub const ETIME: i32 = 62;
    pub const ENOSR: i32 = 63;
    pub const EREMOTE: i32 = 66;
    pub const ENOLINK: i32 = 67;
    pub const EPROTO: i32 = 71;
    pub const EMULTIHOP: i32 = 72;
    pub const EBADMSG: i32 = 74;
    pub const EOVERFLOW: i32 = 75;
    pub const EILSEQ: i32 = 84;
    pub const EUSERS: i32 = 87;
    pub const ENOTSOCK: i32 = 88;
    pub const EDESTADDRREQ: i32 = 89;
    pub const EMSGSIZE: i32 = 90;
    pub const EPROTOTYPE: i32 = 91;
    pub const ENOPROTOOPT: i32 = 92;
    pub const EPROTONOSUPPORT: i32 = 93;
    pub const ESOCKTNOSUPPORT: i32 = 94;
    pub const EOPNOTSUPP: i32 = 95;
    pub const EPFNOSUPPORT: i32 = 96;
    pub const EAFNOSUPPORT: i32 = 97;
    pub const EADDRINUSE: i32 = 98;
    pub const EADDRNOTAVAIL: i32 = 99;
    pub const ENETDOWN: i32 = 100;
    pub const ENETUNREACH: i32 = 101;
    pub const ENETRESET: i32 = 102;
    pub const ECONNABORTED: i32 = 103;
    pub const ECONNRESET: i32 = 104;
    pub const ENOBUFS: i32 = 105;
    pub const EISCONN: i32 = 106;
    pub const ENOTCONN: i32 = 107;
    pub const ESHUTDOWN: i32 = 108;
    pub const ETOOMANYREFS: i32 = 109;
    pub const ETIMEDOUT: i32 = 110;
    pub const ECONNREFUSED: i32 = 111;
    pub const EHOSTDOWN: i32 = 112;
    pub const EHOSTUNREACH: i32 = 113;
    pub const EALREADY: i32 = 114;
    pub const EINPROGRESS: i32 = 115;
    pub const ESTALE: i32 = 116;
    pub const EDQUOT: i32 = 122;
    pub const ECANCELED: i32 = 125;
    pub const EOWNERDEAD: i32 = 130;
    pub const ENOTRECOVERABLE: i32 = 131;
}

/// Each macOS errno this crate translates, with its Linux value.
#[cfg(target_os = "macos")]
const FROM_MACOS: [(usize, i32); 89] = [
    (1, linux::EPERM),
    (2, linux::ENOENT),
    (3, linux::ESRCH),
    (4, linux::EINTR),
    (5, linux::EIO),
    (6, linux::ENXIO),
    (7, linux::E2BIG),
    (8, linux::ENOEXEC),
    (9, linux::EBADF),
    (10, linux::ECHILD),
    (11, linux::EDEADLK),
    (12, linux::ENOMEM),
    (13, linux::EACCES),
    (14, linux::EFAULT),
    (15, linux::ENOTBLK),
    (16, linux::EBUSY),
    (17, linux::EEXIST),
    (18, linux::EXDEV),
    (19, linux::ENODEV),
    (20, linux::ENOTDIR),
    (21, linux::EISDIR),
    (22, linux::EINVAL),
    (23, linux::ENFILE),
    (24, linux::EMFILE),
    (25, linux::ENOTTY),
    (26, linux::ETXTBSY),
    (27, linux::EFBIG),
    (28, linux::ENOSPC),
    (29, linux::ESPIPE),
    (30, linux::EROFS),
    (31, linux::EMLINK),
    (32, linux::EPIPE),
    (33, linux::EDOM),
    (34, linux::ERANGE),
    (35, linux::EAGAIN),
    (36, linux::EINPROGRESS),
    (37, linux::EALREADY),
    (38, linux::ENOTSOCK),
    (39, linux::EDESTADDRREQ),
    (40, linux::EMSGSIZE),
    (41, linux::EPROTOTYPE),
    (42, linux::ENOPROTOOPT),
    (43, linux::EPROTONOSUPPORT),
    (44, linux::ESOCKTNOSUPPORT),
    (45, linux::EOPNOTSUPP),
    (102, linux::EOPNOTSUPP),
    (46, linux::EPFNOSUPPORT),
    (47, linux::EAFNOSUPPORT),
    (48, linux::EADDRINUSE),
    (49, linux::EADDRNOTAVAIL),
    (50, linux::ENETDOWN),
    (51, linux::ENETUNREACH),
    (52, linux::ENETRESET),
    (53, linux::ECONNABORTED),
    (54, linux::ECONNRESET),
    (55, linux::ENOBUFS),
    (56, linux::EISCONN),
    (57, linux::ENOTCONN),
    (58, linux::ESHUTDOWN),
    (59, linux::ETOOMANYREFS),
    (60, linux::ETIMEDOUT),
    (61, linux::ECONNREFUSED),
    (62, linux::ELOOP),
    (63, linux::ENAMETOOLONG),
    (64, linux::EHOSTDOWN),
    (65, linux::EHOSTUNREACH),
    (66, linux::ENOTEMPTY),
    (68, linux::EUSERS),
    (69, linux::EDQUOT),
    (70, linux::ESTALE),
    (71, linux::EREMOTE),
    (77, linux::ENOLCK),
    (78, linux::ENOSYS),
    (84, linux::EOVERFLOW),
    (89, linux::ECANCELED),
    (90, linux::EIDRM),
    (91, linux::ENOMSG),
    (92, linux::EILSEQ),
    (93, linux::ENODATA),
    (96, linux::ENODATA),
    (94, linux::EBADMSG),
    (95, linux::EMULTIHOP),
    (97, linux::ENOLINK),
    (98, linux::ENOSR),
    (99, linux::ENOSTR),
    (100, linux::EPROTO),
    (101, linux::ETIME),
    (104, linux::ENOTRECOVERABLE),
    (105, linux::EOWNERDEAD),
];

/// [`FROM_MACOS`] as a table indexed by the macOS errno; 0 marks an errno
/// without a Linux counterpart.
#[cfg(target_os = "macos")]
const MACOS_TABLE: [i32; 106] = {
    let mut table = [0; 106];
    let mut i = 0;
    while i < FROM_MACOS.len() {
        let (macos, code) = FROM_MACOS[i];
        table[macos] = code;
        i += 1;
    }
    table
};

/// Convert an OS `errno` into its negated Linux value (`-EIO` if unknown).
#[cfg(target_os = "macos")]
pub fn to_linux_errno(os_errno: i32) -> i32 {
    let code = usize::try_from(os_errno)
        .ok()
        .and_then(|at| MACOS_TABLE.get(at).copied())
        .filter(|&code| code != 0)
        .unwrap_or(linux::EIO);
    -code
}

/// The Linux errnos named in [`linux`], passed through unchanged.
#[cfg(target_os = "linux")]
const KNOWN: [i32; 87] = [
    linux::EPERM,
    linux::ENOENT,
    linux::ESRCH,
    linux::EINTR,
    linux::EIO,
    linux::ENXIO,
    linux::E2BIG,
    linux::ENOEXEC,
    linux::EBADF,
    linux::ECHILD,
    linux::EAGAIN,
    linux::ENOMEM,
    linux::EACCES,
    linux::EFAULT,
    linux::ENOTBLK,
    linux::EBUSY,
    linux::EEXIST,
    linux::EXDEV,
    linux::ENODEV,
    linux::ENOTDIR,
    linux::EISDIR,
    linux::EINVAL,
    linux::ENFILE,
    linux::EMFILE,
    linux::ENOTTY,
    linux::ETXTBSY,
    linux::EFBIG,
    linux::ENOSPC,
    linux::ESPIPE,
    linux::EROFS,
    linux::EMLINK,
    linux::EPIPE,
    linux::EDOM,
    linux::ERANGE,
    linux::EDEADLK,
    linux::ENAMETOOLONG,
    linux::ENOLCK,
    linux::ENOSYS,
    linux::ENOTEMPTY,
    linux::ELOOP,
    linux::ENOMSG,
    linux::EIDRM,
    linux::ENOSTR,
    linux::ENODATA,
    linux::ETIME,
    linux::ENOSR,
    linux::EREMOTE,
    linux::ENOLINK,
    linux::EPROTO,
    linux::EMULTIHOP,
    linux::EBADMSG,
    linux::EOVERFLOW,
    linux::EILSEQ,
    linux::EUSERS,
    linux::ENOTSOCK,
    linux::EDESTADDRREQ,
    linux::EMSGSIZE,
    linux::EPROTOTYPE,
    linux::ENOPROTOOPT,
    linux::EPROTONOSUPPORT,
    linux::ESOCKTNOSUPPORT,
    linux::EOPNOTSUPP,
    linux::EPFNOSUPPORT,
    linux::EAFNOSUPPORT,
    linux::EADDRINUSE,
    linux::EADDRNOTAVAIL,
    linux::ENETDOWN,
    linux::ENETUNREACH,
    linux::ENETRESET,
    linux::ECONNABORTED,
    linux::ECONNRESET,
    linux::ENOBUFS,
    linux::EISCONN,
    linux::ENOTCONN,
    linux::ESHUTDOWN,
    linux::ETOOMANYREFS,
    linux::ETIMEDOUT,
    linux::ECONNREFUSED,
    linux::EHOSTDOWN,
    linux::EHOSTUNREACH,
    linux::EALREADY,
    linux::EINPROGRESS,
    linux::ESTALE,
    linux::EDQUOT,
    linux::ECANCELED,
    linux::EOWNERDEAD,
    linux::ENOTRECOVERABLE,
];

/// Convert an OS `errno` into its negated Linux value (`-EIO` if unknown).
#[cfg(target_os = "linux")]
pub fn to_linux_errno(os_errno: i32) -> i32 {
    let code = if KNOWN.contains(&os_errno) {
        os_errno
    } else {
        linux::EIO
    };
    -code
}
