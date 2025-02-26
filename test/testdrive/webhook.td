# Copyright Materialize, Inc. and contributors. All rights reserved.
#
# Use of this software is governed by the Business Source License
# included in the LICENSE file at the root of this repository.
#
# As of the Change Date specified in that file, in accordance with
# the Business Source License, use of this software will be governed
# by the Apache License, Version 2.0.

# Exercies Webhook sources.

$ postgres-execute connection=postgres://mz_system:materialize@${testdrive.materialize-internal-sql-addr}
ALTER SYSTEM SET enable_webhook_sources = true

> CREATE CLUSTER webhook_cluster REPLICAS (r1 (SIZE '1'));

> CREATE SOURCE webhook_text IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT TEXT;

> SHOW COLUMNS FROM webhook_text;
name   nullable  type
---------------------
body   false     text

$ webhook-append database=materialize schema=public name=webhook_text
a

$ webhook-append database=materialize schema=public name=webhook_text
b

$ webhook-append database=materialize schema=public name=webhook_text
c

> SELECT * FROM webhook_text;
a
b
c

> CREATE SOURCE webhook_json_with_headers IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT JSON
  INCLUDE HEADERS;

> SHOW COLUMNS FROM webhook_json_with_headers;
name     nullable  type
------------------------
body     false     jsonb
headers  false     map

$ webhook-append database=materialize schema=public name=webhook_json_with_headers content-type=application/json app=test_drive
{
  "hello": "world"
}

$ webhook-append database=materialize schema=public name=webhook_json_with_headers content-type=application/json app=other
{
  "goodbye": "world"
}

> SELECT body FROM webhook_json_with_headers WHERE headers -> 'app' = 'test_drive';
"{\"hello\":\"world\"}"

$ webhook-append database=materialize schema=public name=webhook_json_with_headers content-type=application/json app=
{
  "empty": "world"
}

> SELECT body FROM webhook_json_with_headers WHERE headers -> 'app' = '';
"{\"empty\":\"world\"}"

$ webhook-append database=materialize schema=public name=webhook_json_with_headers content-type=application/json app=list
[1, 2, 3]

> SELECT body FROM webhook_json_with_headers WHERE headers -> 'app' = 'list';
"[1,2,3]"

$ webhook-append database=materialize schema=public name=webhook_json_with_headers content-type=application/json app=list2
{ "foo": [1, 2, 3] }

> SELECT body FROM webhook_json_with_headers WHERE headers -> 'app' = 'list2';
"{\"foo\":[1,2,3]}"

$ webhook-append database=materialize schema=public name=webhook_json_with_headers content-type=application/json app=string
"Hellö String"

> SELECT body FROM webhook_json_with_headers WHERE headers -> 'app' = 'string';
"\"Hellö String\""

! SELECT * FROM webhook_json_with_headers;
contains: binary encoding of map types is not implemented

# An invalid body should return a 400.
$ webhook-append database=materialize schema=public name=webhook_json_with_headers status=400 content-type=application/json
invalid-json

# A source that doesn't exist should return a 404.
$ webhook-append database=materialize schema=public name=non_existent_source status=404
x

# Trying to append to an object that isn't a webhook should fail.
> CREATE TABLE not_a_webhook ( a int8 );
$ webhook-append database=materialize schema=public name=not_a_webhook status=404
d

> SELECT * FROM webhook_text;
a
b
c

> CREATE SOURCE webhook_bytes IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT BYTES;

> SHOW COLUMNS FROM webhook_bytes;
name   nullable  type
---------------------
body   false     bytea

$ webhook-append database=materialize schema=public name=webhook_bytes
和製漢語

$ webhook-append database=materialize schema=public name=webhook_bytes
null

$ webhook-append database=materialize schema=public name=webhook_bytes
123

> SELECT * FROM webhook_bytes;
"\\xe5\\x92\\x8c\\xe8\\xa3\\xbd\\xe6\\xbc\\xa2\\xe8\\xaa\\x9e"
null
123

> CREATE SOURCE webhook_bytes_with_validation IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT BYTES
  CHECK (
    WITH (HEADERS)
    headers->'signature' = 'test'
  );

$ webhook-append database=materialize schema=public name=webhook_bytes_with_validation signature=test
123

$ webhook-append database=materialize schema=public name=webhook_bytes_with_validation signature=invalid status=400
456

> CREATE SOURCE webhook_bytes_with_hmac IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT BYTES
  CHECK (
    WITH (HEADERS, BODY)
    decode(headers->'x-signature', 'base64') = hmac('body=' || body, 'test_key', 'sha256')
  );

$ webhook-append name=webhook_bytes_with_hmac x-signature=HA0rQdPkCiNNNAladA0eTI8x5WZp5z8rBawQHiywznI=
hello world

$ webhook-append name=webhook_bytes_with_hmac x-signature=1cDmmXBhApqXZebb2u6WtdwHc2UtkMf7N11Zjk66wzo=
another_request

$ webhook-append name=webhook_bytes_with_hmac status=400
did_not_include_necessary_header

> SELECT * FROM webhook_bytes_with_hmac;
"hello world"
"another_request"

> CREATE SECRET webhook_secret AS 'shared_key';

> CREATE SOURCE webhook_bytes_with_secret IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT TEXT
  CHECK (
    WITH ( HEADERS, BODY BYTES, SECRET webhook_secret BYTES )
    decode(headers->'x-signature', 'base64') = hmac(body, webhook_secret, 'sha256')
  )

$ webhook-append name=webhook_bytes_with_secret x-signature=VNCe6bTKrlFO46GfiUYR/xFpeZ2H/KbLfR9oJKYAwkc=
using an mz secret

> SELECT * FROM webhook_bytes_with_secret;
"using an mz secret"

> CREATE SOURCE webhook_buildkite IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT TEXT
  CHECK (
    WITH (HEADERS, BODY)
    decode(split_part(headers->'x-buildkite-signature', 'signature::', 2), 'hex')
      =
    hmac(split_part(split_part(headers->'x-buildkite-signature', 'timestamp::', 2), ',', 1) || '.' || body, 'test_key', 'sha256')
  );

$ webhook-append name=webhook_buildkite x-buildkite-signature=timestamp::42,signature::b610a43432fe965eb8e2a3ce4939a6bafaad3f35583c596e2f7271125a346d95
i hope this works

> SELECT * FROM webhook_buildkite;
"i hope this works"

> CREATE SOURCE webhook_hex IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT TEXT
  CHECK ( WITH (BODY) decode(body, 'hex') = '\x42' );

$ webhook-append name=webhook_hex status=400
# 'z' is an invalid character in hex which causes an evaluation failure.
z

# Enable unstable dependencies so we can use mz_panic.
$ postgres-execute connection=postgres://mz_system:materialize@${testdrive.materialize-internal-sql-addr}
ALTER SYSTEM SET enable_unstable_dependencies = true;

# Create a source that will panic when we run validation. Validation panicking should not take down
# all of environmentd, we should catch the panic.
#
# Note: if you change the message in the panic, then you need to update ci_logged_errors_detect.py.
> CREATE SOURCE webhook_validation_panic IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT TEXT
  CHECK (
    WITH (HEADERS)
    mz_internal.mz_panic('webhook panic test') = headers::text
  );

$ webhook-append name=webhook_validation_panic status=500
abc

# Can use SECRETs as both Bytes and Strings.

> CREATE SECRET webhook_secret_bytes AS 'this_key_is_bytes';

> CREATE SOURCE webhook_double_validation IN CLUSTER webhook_cluster FROM WEBHOOK
  BODY FORMAT TEXT
  CHECK (
    WITH (
      HEADERS,
      BODY BYTES,
      SECRET webhook_secret,
      SECRET webhook_secret_bytes BYTES
    )
    decode(headers->'x-signature-sha256', 'hex') = hmac(convert_from(body, 'utf-8'), webhook_secret, 'sha256')
      AND
    decode(headers->'x-signature-md5', 'hex') = hmac(body, webhook_secret_bytes, 'md5')
  )

$ webhook-append name=webhook_double_validation x-signature-sha256=20460da764521c155989f9ede00d6047c459c87bca6712eef27f72ae32c62d3f x-signature-md5=c34fd128f787067796212d31fced1881
materialize space monkey

> SELECT * FROM webhook_double_validation;
"materialize space monkey"

# Webhooks should support special characters like a /

> CREATE SOURCE "webhook_with_/" IN CLUSTER webhook_cluster FROM WEBHOOK BODY FORMAT TEXT;

$ webhook-append name=webhook_with_/ status=404
wont_work

$ webhook-append name=webhook_with_%2F
will_work

> SELECT * FROM "webhook_with_/"
"will_work"

# Dropping a webhook source should drop the underlying persist shards.

$ set-from-sql var=webhook-source-id
SELECT id FROM mz_sources WHERE name = 'webhook_bytes';

> SELECT COUNT(*) FROM mz_internal.mz_storage_shards WHERE object_id = '${webhook-source-id}';
1

> DROP SOURCE webhook_bytes;

> SELECT COUNT(*) FROM mz_internal.mz_storage_shards WHERE object_id = '${webhook-source-id}';
0

# Turn off the feature.
$ postgres-execute connection=postgres://mz_system:materialize@${testdrive.materialize-internal-sql-addr}
ALTER SYSTEM SET enable_webhook_sources = false

# Appending should now fail because the feature isn't enabled.
$ webhook-append database=materialize schema=public name=webhook_text status=400
d

# Cleanup.
DROP CLUSTER webhook_cluster CASCADE;

$ postgres-execute connection=postgres://mz_system:materialize@${testdrive.materialize-internal-sql-addr}
ALTER SYSTEM SET enable_unstable_dependencies = false;
