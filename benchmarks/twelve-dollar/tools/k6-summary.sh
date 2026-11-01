valid_k6_request_metrics() {
  jaq -e '
    .metrics as $m |
    [($m.http_req_duration.med // $m.http_req_duration.values.med),
     ($m.http_req_duration["p(95)"] // $m.http_req_duration.values["p(95)"]),
     ($m.http_req_duration["p(99)"] // $m.http_req_duration.values["p(99)"]),
     ($m.http_req_duration.max // $m.http_req_duration.values.max),
     ($m.http_req_failed.value // $m.http_req_failed.values.rate),
     ($m.http_reqs.count // $m.http_reqs.values.count),
     ($m.http_reqs.rate // $m.http_reqs.values.rate)] |
    all(.[]; type == "number" and . >= 0) and
    (($m.http_reqs.count // $m.http_reqs.values.count) > 0) and
    (($m.http_req_failed.value // $m.http_req_failed.values.rate) <= 1)
  ' "$1" >/dev/null 2>&1
}

valid_k6_generator_metrics() {
  valid_k6_request_metrics "$1" && jaq -e '
    .metrics.vus_max.max | type == "number" and . >= 0 and floor == .
  ' "$1" >/dev/null 2>&1
}
