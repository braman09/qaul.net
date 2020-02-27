import Service from '@ember/service';
import { inject as service } from '@ember/service';
// function openWs() {
//   console.log('go WS');
//   const webSocket = new WebSocket('ws://localhost:9900');

//   webSocket.onopen = () => {
//     console.log('OPEN');
//     webSocket.send(JSON.stringify({
//       id: 'a', kind: 'users', method: 'list',
//     }));

//     webSocket.onmessage = ({ data }) => {
//       console.log('got', data);
//     }
//   }
// }

// openWs();

export default class BackendService extends Service {
  @service session;

  idx = 0;
  requests = new Map();
  pendingRequests = [];

  constructor() {
    super(...arguments);

    this.loadWs();
  }

  loadWs() {
    const ws = new WebSocket('ws://localhost:9900');
    ws.onopen = () => {
      this.ws = ws;
      console.log('ws ready');
      this.flush();
    }
    ws.onmessage = ({ data }) => this.handleMessage(JSON.parse(data));
  }

  handleMessage({ data, id }) {
    console.log(`${id.padStart(5, ' ')}|> response`);
    const req = this.requests.get(id);
    req.responseData = data;
    req.resolveHandler(data);
  }

  flush() {
    if(this.ws) {
      const id = this.pendingRequests.pop();
      if(id) {
        const { requestKind, requestMethod, requestData } = this.requests.get(id);

        const auth = this.session.isAuthenticated
          ? this.session.data.authenticated.auth
          : undefined;
        this.ws.send(JSON.stringify({
          id,
          auth,
          kind: requestKind,
          method: requestMethod,
          data: requestData,
        }));
        this.flush(); // recursion
      }
    }
  }

  request(requestKind, requestMethod, requestData) {
    return new Promise((resolveHandler, rejectHandler) => {
      const id = `${this.idx++}`;
      console.log(`${id.padStart(5, ' ')}|> request ${requestKind}:${requestMethod}`);

      this.requests.set(id, {
        id,
        requestKind,
        requestMethod,
        requestData,
        resolveHandler,
        rejectHandler,
      });
      this.pendingRequests.push(id);
      this.flush();
    });
  }
}
