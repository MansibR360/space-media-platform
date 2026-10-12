import http from 'k6/http';
import { check, sleep } from 'k6';

// Concurrent-user load test: browse-heavy mix with auth + discovery.
// Run: k6 run tests/load/browse_load.js
export const options = {
  scenarios: {
    browse: { executor: 'ramping-vus', startVUs: 0, stages: [
      { duration: '1m', target: 100 },
      { duration: '5m', target: 500 },
      { duration: '2m', target: 0 },
    ]},
  },
  thresholds: {
    http_req_duration: ['p(95)<400'],
    checks: ['rate>0.99'],
  },
};

const BASE = __ENV.BASE || 'http://localhost:8080';

export function setup() {
  const res = http.post(`${BASE}/api/auth/register`, JSON.stringify({
    username: `load_${Math.random().toString(36).slice(2, 10)}`,
    email: `load_${Math.random().toString(36).slice(2)}@example.test`,
    password: 'Loadtest123',
  }), { headers: { 'Content-Type': 'application/json' } });
  const body = res.json();
  return { token: body.access_token };
}

export default function (data) {
  const headers = { Authorization: `Bearer ${data.token}` };

  let r = http.get(`${BASE}/api/home`, { headers });
  check(r, { 'home 200': (res) => res.status === 200 });

  r = http.get(`${BASE}/api/discover?sort=popularity&limit=24`, { headers });
  check(r, { 'discover 200': (res) => res.status === 200 });

  r = http.get(`${BASE}/api/trending?window=week`);
  check(r, { 'trending 200': (res) => res.status === 200 });

  r = http.get(`${BASE}/api/search?q=${encodeURIComponent('dark thriller')}`, { headers });
  check(r, { 'search 200': (res) => res.status === 200 });

  sleep(Math.random() * 3 + 1);
}
