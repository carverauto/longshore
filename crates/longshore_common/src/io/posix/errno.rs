// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Translates OS `errno` values into negated Linux errno values (`-EIO` fallback).

/// Linux errno values, by name, on every platform.
pub mod linux {
    /// Operation not permitted.
    pub const EPERM: i32 = 1;
    /// No such file or directory.
    pub const ENOENT: i32 = 2;
    /// No such process.
    pub const ESRCH: i32 = 3;
    /// Interrupted by a signal.
    pub const EINTR: i32 = 4;
    /// Input/output error.
    pub const EIO: i32 = 5;
    /// No such device or address.
    pub const ENXIO: i32 = 6;
    /// Argument list too long.
    pub const E2BIG: i32 = 7;
    /// Not an executable format.
    pub const ENOEXEC: i32 = 8;
    /// Bad file descriptor.
    pub const EBADF: i32 = 9;
    /// No child processes.
    pub const ECHILD: i32 = 10;
    /// Nothing is available right now; try again.
    pub const EAGAIN: i32 = 11;
    /// Out of memory.
    pub const ENOMEM: i32 = 12;
    /// Permission denied.
    pub const EACCES: i32 = 13;
    /// Bad address.
    pub const EFAULT: i32 = 14;
    /// A block device is required.
    pub const ENOTBLK: i32 = 15;
    /// The device or resource is busy.
    pub const EBUSY: i32 = 16;
    /// The file exists.
    pub const EEXIST: i32 = 17;
    /// The link would cross file systems.
    pub const EXDEV: i32 = 18;
    /// No such device.
    pub const ENODEV: i32 = 19;
    /// Not a directory.
    pub const ENOTDIR: i32 = 20;
    /// Is a directory.
    pub const EISDIR: i32 = 21;
    /// Invalid argument.
    pub const EINVAL: i32 = 22;
    /// Too many open files in the system.
    pub const ENFILE: i32 = 23;
    /// Too many open files in the process.
    pub const EMFILE: i32 = 24;
    /// The request does not apply to the device.
    pub const ENOTTY: i32 = 25;
    /// The program file is being executed.
    pub const ETXTBSY: i32 = 26;
    /// The file is too large.
    pub const EFBIG: i32 = 27;
    /// No space left on the device.
    pub const ENOSPC: i32 = 28;
    /// The file cannot seek.
    pub const ESPIPE: i32 = 29;
    /// The file system is read-only.
    pub const EROFS: i32 = 30;
    /// Too many links.
    pub const EMLINK: i32 = 31;
    /// Broken pipe.
    pub const EPIPE: i32 = 32;
    /// The argument is outside the function's domain.
    pub const EDOM: i32 = 33;
    /// The result cannot be represented.
    pub const ERANGE: i32 = 34;
    /// The lock would deadlock.
    pub const EDEADLK: i32 = 35;
    /// The file name is too long.
    pub const ENAMETOOLONG: i32 = 36;
    /// No locks are available.
    pub const ENOLCK: i32 = 37;
    /// The function is not implemented.
    pub const ENOSYS: i32 = 38;
    /// The directory is not empty.
    pub const ENOTEMPTY: i32 = 39;
    /// Too many symbolic links.
    pub const ELOOP: i32 = 40;
    /// No message of the desired type.
    pub const ENOMSG: i32 = 42;
    /// The identifier was removed.
    pub const EIDRM: i32 = 43;
    /// The device is not a stream.
    pub const ENOSTR: i32 = 60;
    /// No data is available.
    pub const ENODATA: i32 = 61;
    /// A timer expired.
    pub const ETIME: i32 = 62;
    /// Out of stream resources.
    pub const ENOSR: i32 = 63;
    /// The object is remote.
    pub const EREMOTE: i32 = 66;
    /// The link was severed.
    pub const ENOLINK: i32 = 67;
    /// Protocol error.
    pub const EPROTO: i32 = 71;
    /// A multihop was attempted.
    pub const EMULTIHOP: i32 = 72;
    /// Bad message.
    pub const EBADMSG: i32 = 74;
    /// The value is too large for its type.
    pub const EOVERFLOW: i32 = 75;
    /// Invalid byte sequence.
    pub const EILSEQ: i32 = 84;
    /// Too many users.
    pub const EUSERS: i32 = 87;
    /// Not a socket.
    pub const ENOTSOCK: i32 = 88;
    /// A destination address is required.
    pub const EDESTADDRREQ: i32 = 89;
    /// The message is too long.
    pub const EMSGSIZE: i32 = 90;
    /// The protocol does not fit the socket's type.
    pub const EPROTOTYPE: i32 = 91;
    /// The protocol option is not available.
    pub const ENOPROTOOPT: i32 = 92;
    /// The protocol is not supported.
    pub const EPROTONOSUPPORT: i32 = 93;
    /// The socket type is not supported.
    pub const ESOCKTNOSUPPORT: i32 = 94;
    /// The operation is not supported.
    pub const EOPNOTSUPP: i32 = 95;
    /// The protocol family is not supported.
    pub const EPFNOSUPPORT: i32 = 96;
    /// The address family is not supported.
    pub const EAFNOSUPPORT: i32 = 97;
    /// The address is in use.
    pub const EADDRINUSE: i32 = 98;
    /// The address is not available.
    pub const EADDRNOTAVAIL: i32 = 99;
    /// The network is down.
    pub const ENETDOWN: i32 = 100;
    /// The network is unreachable.
    pub const ENETUNREACH: i32 = 101;
    /// The network dropped the connection on reset.
    pub const ENETRESET: i32 = 102;
    /// The connection was aborted.
    pub const ECONNABORTED: i32 = 103;
    /// The peer reset the connection.
    pub const ECONNRESET: i32 = 104;
    /// No buffer space is available.
    pub const ENOBUFS: i32 = 105;
    /// The socket is connected.
    pub const EISCONN: i32 = 106;
    /// The socket is not connected.
    pub const ENOTCONN: i32 = 107;
    /// The socket was shut down for sending.
    pub const ESHUTDOWN: i32 = 108;
    /// Too many references.
    pub const ETOOMANYREFS: i32 = 109;
    /// The connection timed out.
    pub const ETIMEDOUT: i32 = 110;
    /// The connection was refused.
    pub const ECONNREFUSED: i32 = 111;
    /// The host is down.
    pub const EHOSTDOWN: i32 = 112;
    /// No route to the host.
    pub const EHOSTUNREACH: i32 = 113;
    /// The operation is already in progress.
    pub const EALREADY: i32 = 114;
    /// The operation is now in progress.
    pub const EINPROGRESS: i32 = 115;
    /// The file handle is stale.
    pub const ESTALE: i32 = 116;
    /// The disk quota was exceeded.
    pub const EDQUOT: i32 = 122;
    /// The operation was cancelled.
    pub const ECANCELED: i32 = 125;
    /// The lock's owner died.
    pub const EOWNERDEAD: i32 = 130;
    /// The state cannot be recovered.
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
