"""Layer 3 — classifier + policy unit tests (fast, no container, no network).

The security property under test is *negative*: every allow has a paired deny.
Run: `python3 -m pytest test_policy.py` from this directory.
"""
import pytest

from policy import READ, WRITE, UNKNOWN, Action, Policy, Request, classify, decide

# The cluster API server host used across the k8s cases.
K8S = "k8s.example.internal"


def req(host, method="GET", path="/", headers=None, body=b""):
    return Request(host=host, method=method, path=path, headers=headers or {}, body=body)


# ---- AWS ---------------------------------------------------------------------


def test_aws_ec2_describe_is_read():
    # ec2 is query-protocol: Action= lives in the POST body.
    r = req("ec2.us-east-1.amazonaws.com", "POST", "/",
            body=b"Action=DescribeInstances&Version=2016-11-15")
    a = classify(r)
    assert a.service == "aws:ec2"
    assert a.verb == "DescribeInstances"
    assert a.kind == READ


def test_aws_ec2_terminate_is_write():
    r = req("ec2.us-east-1.amazonaws.com", "POST", "/",
            body=b"Action=TerminateInstances&InstanceId.1=i-123")
    assert classify(r).kind == WRITE


def test_aws_json_protocol_x_amz_target_read_and_write():
    get = req("dynamodb.us-east-1.amazonaws.com", "POST", "/",
              headers={"X-Amz-Target": "DynamoDB_20120810.GetItem"})
    delete = req("dynamodb.us-east-1.amazonaws.com", "POST", "/",
                 headers={"X-Amz-Target": "DynamoDB_20120810.DeleteTable"})
    assert classify(get).kind == READ
    assert classify(delete).kind == WRITE


def test_aws_action_in_query_string():
    r = req("sts.amazonaws.com", "GET", "/?Action=GetCallerIdentity&Version=2011-06-15")
    assert classify(r).verb == "GetCallerIdentity"
    assert classify(r).kind == READ


# ---- kubectl -----------------------------------------------------------------


def test_k8s_get_is_read():
    r = req(K8S, "GET", "/api/v1/namespaces/prod/pods")
    a = classify(r, k8s_hosts={K8S})
    assert a.service == "k8s"
    assert a.kind == READ


def test_k8s_delete_is_write():
    r = req(K8S, "DELETE", "/api/v1/namespaces/prod/pods/web-0")
    assert classify(r, k8s_hosts={K8S}).kind == WRITE


def test_k8s_watch_is_read():
    r = req(K8S, "GET", "/api/v1/pods?watch=true")
    assert classify(r, k8s_hosts={K8S}).kind == READ


# ---- GitHub / gh -------------------------------------------------------------


def test_github_rest_get_is_read_delete_is_write():
    assert classify(req("api.github.com", "GET", "/repos/o/r")).kind == READ
    assert classify(req("api.github.com", "DELETE", "/repos/o/r")).kind == WRITE


def test_github_graphql_query_is_read():
    r = req("api.github.com", "POST", "/graphql",
            body=b'{"query":"query { viewer { login } }"}')
    a = classify(r)
    assert a.verb == "graphql:query"
    assert a.kind == READ


def test_github_graphql_mutation_is_write():
    r = req("api.github.com", "POST", "/graphql",
            body=b'{"query":"mutation { mergePullRequest(input:{}) { clientMutationId } }"}')
    assert classify(r).kind == WRITE


# ---- decide(): the read-only policy ------------------------------------------


@pytest.fixture
def read_only():
    return Policy(
        allow_hosts={"*.amazonaws.com", "api.github.com", K8S},
        k8s_hosts={K8S},
        mode="read_only",
    )


def test_decide_allows_reads(read_only):
    r = req("ec2.us-east-1.amazonaws.com", "POST", "/", body=b"Action=DescribeInstances")
    assert decide(r, read_only).allow is True


def test_decide_blocks_writes(read_only):
    r = req(K8S, "DELETE", "/api/v1/namespaces/prod/pods/web-0")
    d = decide(r, read_only)
    assert d.allow is False
    assert "blocked" in d.reason


def test_decide_blocks_off_allowlist_host(read_only):
    d = decide(req("evil.example.com", "GET", "/"), read_only)
    assert d.allow is False
    assert "not in allowlist" in d.reason


def test_decide_fails_closed_on_unknown_action(read_only):
    # Allowed host, but no recognised action shape -> deny, don't guess.
    r = req("ec2.us-east-1.amazonaws.com", "POST", "/", body=b"garbage")
    d = decide(r, read_only)
    assert d.allow is False
    assert "fail closed" in d.reason


def test_decide_explicit_allow_verb_overrides_write():
    policy = Policy(
        allow_hosts={"*.amazonaws.com"},
        mode="read_only",
        allow_verbs={"TerminateInstances"},
    )
    r = req("ec2.us-east-1.amazonaws.com", "POST", "/", body=b"Action=TerminateInstances")
    assert decide(r, policy).allow is True


def test_decide_explicit_deny_verb_overrides_read():
    policy = Policy(allow_hosts={"api.github.com"}, deny_verbs={"GET /repos/o/secret"})
    r = req("api.github.com", "GET", "/repos/o/secret")
    assert decide(r, policy).allow is False


def test_decide_mode_all_allows_write_on_allowed_host():
    policy = Policy(allow_hosts={"*.amazonaws.com"}, mode="all")
    r = req("ec2.us-east-1.amazonaws.com", "POST", "/", body=b"Action=TerminateInstances")
    assert decide(r, policy).allow is True
