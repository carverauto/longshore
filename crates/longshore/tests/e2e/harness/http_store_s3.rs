// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! In-memory S3 endpoint dispatch for `http_store`.

use super::{StoreObject, StoreState};
use longshore::checksum::{crc64_header_value, crc64_nvme};
use std::collections::BTreeMap;

pub(super) fn store_put(st: &mut StoreState, key: &str, data: Vec<u8>) -> StoreObject {
    let crc = crc64_nvme(&data);
    let (etag, crc64_b64) = (format!("\"{crc:016x}\""), crc64_header_value(crc));
    let obj = StoreObject {
        data,
        etag,
        crc64_b64,
    };
    st.objects.insert(key.to_string(), obj.clone());
    obj
}

fn qp<'a>(q: &'a str, n: &str) -> Option<&'a str> {
    q.split('&')
        .find_map(|kv| kv.split_once('=').and_then(|(k, v)| (k == n).then_some(v)))
}

fn resp(status: &str, hdrs: &str, body: &[u8]) -> Vec<u8> {
    let len = body.len();
    let mut raw = format!("HTTP/1.1 {status}\r\nContent-Length: {len}\r\n{hdrs}\r\n").into_bytes();
    raw.extend_from_slice(body);
    raw
}

fn ok(hdrs: &str, body: &[u8]) -> Vec<u8> {
    resp("200 OK", hdrs, body)
}

fn xml_ok(xml: String) -> Vec<u8> {
    ok("Content-Type: application/xml\r\n", xml.as_bytes())
}

fn obj_hdrs(o: &StoreObject, extra: &str) -> String {
    let crc = std::str::from_utf8(&o.crc64_b64).unwrap_or("");
    format!(
        "{extra}ETag: {}\r\nx-amz-checksum-crc64nvme: {crc}\r\n",
        o.etag
    )
}

pub(super) fn dispatch_s3(
    st: &mut StoreState,
    m: &str,
    target: &str,
    inm: bool,
    range: Option<(usize, usize)>,
    body: Vec<u8>,
) -> Vec<u8> {
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let key = path.trim_start_matches('/');
    let no_up = || {
        resp(
            "404 Not Found",
            "",
            b"<Error><Code>NoSuchUpload</Code></Error>",
        )
    };
    match m {
        "POST" if q.contains("uploads") => {
            st.next_id += 1;
            let id = format!("up-{}", st.next_id);
            st.uploads
                .insert(id.clone(), (key.to_string(), BTreeMap::new()));
            xml_ok(format!("<InitiateMultipartUploadResult><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"))
        }
        "POST" if qp(q, "uploadId").is_some() => {
            match st.uploads.remove(qp(q, "uploadId").unwrap_or("")) {
                Some((k, parts)) => {
                    let o = store_put(st, &k, parts.into_values().flatten().collect());
                    xml_ok(format!("<CompleteMultipartUploadResult><ETag>{}</ETag></CompleteMultipartUploadResult>", o.etag))
                }
                None => no_up(),
            }
        }
        "PUT" if qp(q, "partNumber").is_some() => {
            let p: u32 = qp(q, "partNumber")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            match st.uploads.get_mut(qp(q, "uploadId").unwrap_or("")) {
                Some((_, parts)) => {
                    parts.insert(p, body);
                    ok(&format!("ETag: \"part-{p}\"\r\n"), &[])
                }
                None => no_up(),
            }
        }
        "PUT" if inm && st.objects.contains_key(key) => resp("412 Precondition Failed", "", &[]),
        "PUT" => ok(&obj_hdrs(&store_put(st, key, body), ""), &[]),
        "GET" if q.contains("list-type=2") => {
            let pfx = qp(q, "prefix").unwrap_or("");
            let mut xml = String::from("<ListBucketResult><IsTruncated>false</IsTruncated>");
            for (k, o) in st.objects.iter().filter(|(k, _)| k.starts_with(pfx)) {
                xml.push_str(&format!(
                    "<Contents><Key>{k}</Key><Size>{}</Size></Contents>",
                    o.data.len()
                ));
            }
            xml.push_str("</ListBucketResult>");
            xml_ok(xml)
        }
        "GET" => match (st.objects.get(key), range) {
            (None, _) => resp(
                "404 Not Found",
                "",
                b"<Error><Code>NoSuchKey</Code></Error>",
            ),
            (Some(o), Some((a, b))) if a >= o.data.len() || b < a => {
                resp("416 Range Not Satisfiable", "", &[])
            }
            (Some(o), Some((a, b))) => {
                let end = b.min(o.data.len() - 1);
                let cr = format!("Content-Range: bytes {a}-{end}/{}\r\n", o.data.len());
                resp("206 Partial Content", &obj_hdrs(o, &cr), &o.data[a..=end])
            }
            (Some(o), None) => ok(&obj_hdrs(o, ""), &o.data),
        },
        "HEAD" => match st.objects.get(key) {
            Some(o) => {
                let h = obj_hdrs(o, &format!("Content-Length: {}\r\n", o.data.len()));
                format!("HTTP/1.1 200 OK\r\n{h}\r\n").into_bytes()
            }
            None => b"HTTP/1.1 404 Not Found\r\n\r\n".to_vec(),
        },
        "DELETE" => {
            if let Some(id) = qp(q, "uploadId") {
                st.uploads.remove(id);
            } else {
                st.objects.remove(key);
            }
            resp("204 No Content", "", &[])
        }
        _ => resp("405 Method Not Allowed", "", &[]),
    }
}
