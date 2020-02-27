import Controller from '@ember/controller';
import { action } from '@ember/object';
import { inject as service } from '@ember/service';
import { tracked } from '@glimmer/tracking';

export default class RegisterController extends Controller {
  @service session;
  @service backend;

  @tracked password = "";
  @tracked realName = "";
  @tracked displayName = "";

  @action
  async register(event) {
    event.preventDefault();

    // {"id": "1", "kind": "users", "method": "create", "data": { "pw": "foobar" } }

    const { auth } = await this.backend.request('users', 'create', { pw: this.password });
    await this.session.authenticate('authenticator:qaul', auth.id, this.password);

    const user = await this.store.findRecord('user', auth.id);

    // const secretRequest = await fetch('/api/secrets', {
    //   method: 'POST',
    //   headers: { 'Content-Type': 'application/vnd.api+json' },
    //   body: JSON.stringify({
    //     data: {
    //       type: 'secret',
    //       attributes: { value: this.password }
    //     }
    //   })
    // });

    // if(secretRequest.status !== 201) {
    //   throw "can not create secret";
    // }

    // const secretData = await secretRequest.json();
    // const userId = secretData.data.relationships.user.data.id;

    // await this.session.authenticate('authenticator:qaul', userId, this.password);

    // const user = await this.store.findRecord('user', userId);
    // user.realName = this.realName;
    // user.displayName = this.displayName,

    // await user.save();
  }
}
