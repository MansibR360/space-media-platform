import http from 'k6/http';
import { check } from 'k6';

// HTTP range streaming stress — exercises partial content paths & disk IO.
// Run: k6 run -e FILE_ID=<uuid> tests/load/stream_stress.js
export const options = {
  scenarios: {
    streamers: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.RATE || 120),
      timeUnit: '1s',
      duration: '3m',
      preAllocatedVUs: 300,
    },
  },
  thresholds: { http_req_duration: ['p(95)<800'] },
};

const BASE = __ENV.BASE || 'http://localhost:8080';
const FILE_ID = __ENV.FILE_ID;

export function setup() {
  if (!FILE_ID) throw new Error('set -e FILE_ID=<uuid>');
  const res = http.post(`${BASE}/api/auth/register`, JSON.stringify({
    username: `stream_${Math.random().toString(36).slice(2, 10)}`,
    email: `stream_${Math.random().toString(36).slice(2)}@example.test`,
    password: 'Streamtest123',
  }), { headers: { 'Content-Type': 'application/json' } });
  return { token: res.json().access_token };
}

export default function (data) {
  const headers = { Authorization: `Bearer ${data.token}` };

  // Simulate a player fetching segments / ranged chunks
  const start = Math.floor(Math.random() * 4_000_000);
  const chunk = 512 * 1024;
  let r = http.get(`${BASE}/api/files/${FILE_ID}/raw`, Object.assign({
    headers: Object.assign({}, headers, { Range: `bytes=${start}-${start + chunk}` }),
  }));
  check(r, { 'range 206/200': (res) => [200, 206].includes(res.status) });

  r = http.get(`${BASE}/api/files/${FILE_ID}/master.m3u8`, { headers });
  check(r, { 'playlist ok or absent': (res) => [200, 404].includes(res.status) });
}
