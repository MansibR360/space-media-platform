import ws from 'k6/ws';
import { check } from 'k6';
import http from 'k6/http';

// WebSocket connection stress test: N concurrent sockets, subscribe storms,
// presence churn. Run: k6 run -e VUS=2000 tests/load/ws_stress.js
export const options = {
  scenarios: {
    sockets: {
      executor: 'ramping-arrival-rate',
      startRate: 10,
      timeUnit: '1s',
      preAllocatedVUs: 500,
      stages: [
        { duration: '1m', target: Number(__ENV.RATE || 200) },
        { duration: '3m', target: Number(__ENV.RATE || 200) },
      ],
    },
  },
};

const BASE = __ENV.BASE || 'http://localhost:8080';

export function setup() {
  const tokens = [];
  for (let i = 0; i < 50; i++) {
    const res = http.post(`${BASE}/api/auth/register`, JSON.stringify({
      username: `ws_${Math.random().toString(36).slice(2, 10)}`,
      email: `ws_${Math.random().toString(36).slice(2)}@example.test`,
      password: 'Wsstress123',
    }), { headers: { 'Content-Type': 'application/json' } });
    tokens.push(res.json().access_token);
  }
  return { tokens };
}

export default function (data) {
  const token = data.tokens[Math.floor(Math.random() * data.tokens.length)];
  const url = `${BASE.replace('http', 'ws')}/ws?token=${token}`;

  ws.connect(url, {}, (socket) => {
    socket.on('open', () => {
      socket.send(JSON.stringify({ type: 'ping' }));
      socket.send(JSON.stringify({ type: 'subscribe', room: '__global__' }));
      // rapid typing events to stress the broadcast path
      for (let i = 0; i < 20; i++) {
        socket.send(JSON.stringify({ type: 'typing', room: '__global__' }));
      }
      setTimeout(() => socket.close(), 30000 + Math.random() * 30000);
    });
    socket.on('message', () => {});
  });
}
