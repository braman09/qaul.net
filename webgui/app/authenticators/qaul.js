import Base from 'ember-simple-auth/authenticators/base';
import { inject as service } from '@ember/service';

export default class QaulAuthenticator extends Base {
  @service backend;

  restore(data) {
    return data;
  }

  async authenticate(user, password) {
    return await this.backend.request('users', 'login', {
      pw: password,
      user,
    });

    // const grantResponse = await fetch('/api/grants', {
    //   method: 'POST',
    //   headers: { 'Content-Type': 'application/vnd.api+json' },
    //   body: JSON.stringify({
    //       data: {
    //         type: 'grant',
    //         attributes: {
    //           secret: password
    //         },
    //         relationships: {
    //           user: {
    //             data: {
    //               type: 'user',
    //               id: userId
    //             }
    //           }
    //         }
    //       }
    //     })
    // });

    // if(grantResponse.status !== 201) {
    //   throw "can not create grant";
    // }

    // const grantData = await grantResponse.json();
    // const token = grantData.data.id;

    // return {
    //   token,
    //   userId,
    // };
  }

  async invalidate({ token }) {
    await fetch(`/api/grants/${token}`, {
      method: 'DELETE',
      headers: {
        'Content-Type': 'application/vnd.api+json',
        Authorization: `Bearer ${token}`
      },
    });
  }
}
