import Adapter from '@ember-data/adapter';
import { inject as service } from '@ember/service';

export default class ApplicationAdapter extends Adapter {
  @service backend;

  findRecord(store, type, id/*, snapshot */) {
    this.backend.request(type.modelName, 'get', {
      [type.modelName]: id
    });
  }
  // namespace = 'api';

  // @service() session;

  // get headers() {
  //   if(this.session.isAuthenticated) {
  //     return {
  //       Authorization: `Bearer ${this.session.data.authenticated.token}`,
  //     }
  //   }

  //   return {};
  // }
}
