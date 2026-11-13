// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

// A real response from Akamai/Linode Object Storage (us-ord), 2026-11-12.
const LINODE: &str = "<ListBucketResult><Contents><Key>probe/c.txt</Key><LastModified>2026-11-12T18:15:00.458Z</LastModified><ETag>&#34;9dd4e461268c8034f5c8564e155c67a6&#34;</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass><Type>UNSPECIFIED</Type></Contents><Contents><Key>probe/h.txt</Key><LastModified>2026-11-12T18:14:59.962Z</LastModified><ETag>&#34;9dd4e461268c8034f5c8564e155c67a6&#34;</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass><Type>UNSPECIFIED</Type></Contents><Contents><Key>probe/new-20357.txt</Key><LastModified>2026-11-12T18:14:59.690Z</LastModified><ETag>&#34;8b04d5e3775d298e78455efc5ca404d5&#34;</ETag><Size>5</Size><StorageClass>STANDARD</StorageClass><Type>UNSPECIFIED</Type></Contents><Name>bucket</Name><Prefix>probe/</Prefix><NextContinuationToken>probe/new-20357.txt</NextContinuationToken><IsTruncated>true</IsTruncated><MaxKeys>3</MaxKeys><KeyCount>3</KeyCount></ListBucketResult>";

fn keys(xml: &str) -> Result<(Vec<(String, u64)>, bool), ListError> {
    let l = Listing::parse(xml.as_bytes())?;
    let mut v = vec![];
    for e in l.entries() {
        let e = e?;
        v.push((String::from_utf8(e.key_raw.to_vec()).unwrap(), e.size));
    }
    Ok((v, l.is_truncated))
}

#[test]
fn parses_a_real_response() {
    let (k, truncated) = keys(LINODE).unwrap();
    assert_eq!(
        k,
        [
            ("probe/c.txt".to_string(), 1),
            ("probe/h.txt".to_string(), 1),
            ("probe/new-20357.txt".to_string(), 5)
        ]
    );
    assert!(truncated);
}

#[test]
fn accepts_the_aws_shape_with_declaration_namespace_and_field_order() {
    // AWS puts the summary fields first and wraps the document differently.
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>b</Name><Prefix></Prefix><KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated><Contents><Key>a</Key><LastModified>2009-10-12T17:50:30.000Z</LastModified><ETag>\"x\"</ETag><Size>434234</Size><StorageClass>STANDARD</StorageClass></Contents><Contents><Size>0</Size><Key>dir/</Key></Contents></ListBucketResult>";
    assert_eq!(
        keys(xml).unwrap(),
        (
            vec![("a".to_string(), 434234), ("dir/".to_string(), 0)],
            false
        )
    );
}

#[test]
fn an_empty_listing_is_valid() {
    assert_eq!(keys("<ListBucketResult><Name>b</Name><IsTruncated>false</IsTruncated><KeyCount>0</KeyCount></ListBucketResult>").unwrap(), (vec![], false));
}

#[test]
fn a_key_named_like_a_field_does_not_confuse_the_summary() {
    // "IsTruncated" inside a key must not be read as the listing's flag.
    let xml = "<ListBucketResult><Contents><Key>&lt;IsTruncated&gt;true&lt;/IsTruncated&gt;</Key><Size>1</Size></Contents><IsTruncated>false</IsTruncated></ListBucketResult>";
    let (k, truncated) = keys(xml).unwrap();
    assert!(!truncated);
    let mut out = [0u8; 64];
    let n = unescape(k[0].0.as_bytes(), &mut out).unwrap();
    assert_eq!(&out[..n], b"<IsTruncated>true</IsTruncated>");
}

#[test]
fn rejects_other_documents_and_damage() {
    assert_eq!(
        keys("<Error><Code>AccessDenied</Code></Error>"),
        Err(ListError::NotAListing)
    );
    assert_eq!(
        keys("<ListBucketResultX><IsTruncated>false</IsTruncated></ListBucketResultX>"),
        Err(ListError::NotAListing)
    );
    assert_eq!(keys(""), Err(ListError::NotAListing));
    assert_eq!(
        keys("<ListBucketResult><IsTruncated>false</IsTruncated>"),
        Err(ListError::Malformed),
        "no closing root"
    );
    assert_eq!(
        keys("<ListBucketResult><Name>b</Name></ListBucketResult>"),
        Err(ListError::Malformed),
        "no IsTruncated"
    );
    assert_eq!(
        keys("<ListBucketResult><IsTruncated>maybe</IsTruncated></ListBucketResult>"),
        Err(ListError::Malformed)
    );
    assert_eq!(keys("<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>a</Key></Contents></ListBucketResult>"), Err(ListError::Malformed), "no Size");
    assert_eq!(keys("<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>a</Key><Size>1x</Size></Contents></ListBucketResult>"), Err(ListError::Malformed));
    assert_eq!(keys("<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>a</Key><Size>99999999999999999999</Size></Contents></ListBucketResult>"), Err(ListError::Malformed), "size overflows u64");
    assert_eq!(keys("<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>a</Key><Size>1</Size></ListBucketResult>"), Err(ListError::Malformed), "unterminated Contents");
    // Every truncation of a valid document is rejected.
    for cut in 0..LINODE.len() {
        assert!(
            keys(&LINODE[..cut]).is_err(),
            "accepted a document cut at {cut}"
        );
    }
}

#[test]
fn unescapes_character_references() {
    let d = |s: &str| {
        let mut out = [0u8; 64];
        unescape(s.as_bytes(), &mut out).map(|n| String::from_utf8(out[..n].to_vec()).unwrap())
    };
    assert_eq!(d("plain/key.txt").as_deref(), Some("plain/key.txt"));
    assert_eq!(
        d("a&amp;b&lt;c&gt;d&quot;e&apos;f").as_deref(),
        Some("a&b<c>d\"e'f")
    );
    assert_eq!(
        d("&#34;q&#x22;&#233;&#x1F600;").as_deref(),
        Some("\"q\"é😀")
    );
    for bad in [
        "&",
        "&amp",
        "&bogus;",
        "&#;",
        "&#x;",
        "&#xZZ;",
        "&#1114112;",
        "&#xD800;",
        "&#99999999;",
    ] {
        assert_eq!(d(bad), None, "{bad:?}");
    }
    assert_eq!(unescape(b"abc", &mut [0u8; 2]), None, "output too small");
}
