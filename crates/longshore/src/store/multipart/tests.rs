// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn identifiers_refuse_what_does_not_fit() {
    assert!(UploadId::new(&[b'a'; UPLOAD_ID_MAX]).is_some());
    assert!(UploadId::new(&[b'a'; UPLOAD_ID_MAX + 1]).is_none());
    assert!(PartTag::new(&[b'a'; PART_TAG_MAX + 1]).is_none());
    assert!(PartTag::new(b"").is_none());
    assert_eq!(PartTag::new(b"\"abc\"").unwrap().as_bytes(), b"\"abc\"");
}

#[test]
fn finds_the_upload_id_in_a_real_response() {
    // The shape Amazon S3 documents for CreateMultipartUpload.
    let doc = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>bucket</Bucket><Key>a/b</Key><UploadId>VXBsb2Fk.IElE_-</UploadId></InitiateMultipartUploadResult>";
    assert_eq!(upload_id_in(doc), Some(&b"VXBsb2Fk.IElE_-"[..]));
    assert_eq!(upload_id_in(b"<UploadId></UploadId>"), None);
    assert_eq!(upload_id_in(b"<UploadId>abc"), None);
    assert_eq!(
        upload_id_in(b"<UploadIdentifier>x</UploadIdentifier>"),
        None
    );
    assert_eq!(upload_id_in(b""), None);
    assert_eq!(error_code_in(doc), None);
}

#[test]
fn recognises_an_error_document() {
    let doc = b"<?xml version=\"1.0\"?><Error><Code>InternalError</Code><Message>We encountered an internal error.</Message></Error>";
    assert_eq!(error_code_in(doc), Some(&b"InternalError"[..]));
    // An error document with no code is still an error.
    assert_eq!(error_code_in(b"<Error></Error>"), Some(&b""[..]));
    let ok = b"<CompleteMultipartUploadResult><Location>l</Location><ETag>\"e-2\"</ETag></CompleteMultipartUploadResult>";
    assert_eq!(error_code_in(ok), None);
}

#[test]
fn writes_parts_in_order_with_quotes_escaped() {
    let a = PartTag::new(b"\"aa\"").unwrap();
    let b = PartTag::new(b"\"b&b\"").unwrap();
    let mut out = [0u8; 512];
    let n = write_manifest(&mut out, &[(1, a), (7, b)]).unwrap();
    assert_eq!(
        core::str::from_utf8(&out[..n]).unwrap(),
        "<CompleteMultipartUpload>\
         <Part><PartNumber>1</PartNumber><ETag>&quot;aa&quot;</ETag></Part>\
         <Part><PartNumber>7</PartNumber><ETag>&quot;b&amp;b&quot;</ETag></Part>\
         </CompleteMultipartUpload>"
    );
}

#[test]
fn refuses_a_manifest_that_a_store_would_refuse_or_that_does_not_fit() {
    let t = PartTag::new(b"\"t\"").unwrap();
    let mut out = [0u8; 512];
    assert!(write_manifest(&mut out, &[(2, t), (1, t)]).is_none());
    assert!(write_manifest(&mut out, &[(1, t), (1, t)]).is_none());
    assert!(write_manifest(&mut out, &[(0, t)]).is_none());
    assert!(write_manifest(&mut out, &[(PART_NUMBER_MAX + 1, t)]).is_none());
    assert!(write_manifest(&mut out, &[(PART_NUMBER_MAX, t)]).is_some());
    let need = write_manifest(&mut out, &[(1, t)]).unwrap();
    assert!(write_manifest(&mut out[..need - 1], &[(1, t)]).is_none());
    assert!(write_manifest(&mut out[..need], &[(1, t)]).is_some());
}
