use crate::{
    Cookies,
    JsonApi,
    models::{
        UserAuth,
        UserGrant,
        GrantType,
        Success,
    },
    QaulCore,
    JSONAPI_MIME,
};
use cookie::Cookie;
use chrono::{ DateTime, offset::Utc };
use libqaul::UserAuth as QaulUserAuth;
use iron::{
    prelude::*,
    status::Status,
};
use japi::{
    Document,
    OptionalVec,
    ResourceObject,
};
use std::convert::TryInto;
use super::{
    AuthError,
    Authenticator,
    CurrentUser,
};

pub fn login(req: &mut Request) -> IronResult<Response> {
    // data should contain exactly one object
    let data = match &req.extensions.get::<JsonApi>().unwrap().data {
        OptionalVec::One(Some(d)) => d,
        OptionalVec::Many(_) => { 
            return Err(AuthError::MultipleData.into());
        },
        _ => {
            return Err(AuthError::NoData.into());
        },
    };

    // try to decode the payload
    let ua : ResourceObject<UserAuth> = match data.try_into() {
        Ok(ua) => ua,
        Err(e) => {
            return Err(AuthError::ConversionError(e).into());
        },
    };

    // is the identity valid
    let identity = match UserAuth::identity(&ua) {
        Ok(id) => id,
        Err(e) => {
            return Err(AuthError::InvalidIdentity(e).into());
        },
    };

    // is there a secret (there has to be a secret!)
    let attr = match ua.attributes {
        Some(s) => s,
        None => { 
            return Err(AuthError::NoAttributes.into());
        },
    };

    let secret = attr.secret;
    let grant_type = attr.grant_type;

    let qaul = req.extensions.get::<QaulCore>().unwrap();

    // perform the login
    let (ident, token) = match qaul.user_login(identity.clone(), &secret) {
        Ok(QaulUserAuth::Trusted(ident, token)) => (ident, token),
        Ok(QaulUserAuth::Untrusted(_)) => { unreachable!(); },
        Err(e) => {
            return Err(AuthError::QaulError(e).into());
        },
    };

    // register the token with the authenticator
    {
        req.extensions.get::<Authenticator>().unwrap()
            .tokens.lock().unwrap()
            .insert(token.clone(), ident);
    }

    // return the grant
    let obj = match grant_type {
        GrantType::Token => ResourceObject::<UserGrant>::new(token, None).into(),
        GrantType::Cookie => { 
            // TODO: what should we do when a user is already logged in?
            req.extensions.get_mut::<Cookies>().unwrap().add(Cookie::new("bearer", token));
            Success::from_message("Successfully logged in".into()).into()
        },
    };

    let doc = Document {
        data: OptionalVec::One(Some(obj)),
        ..Default::default()
    };

    Ok(Response::with((Status::Ok, JSONAPI_MIME.clone(), serde_json::to_string(&doc).unwrap())))
}

#[cfg(test)]
mod test {
    use anneal::RequestBuilder;
    use crate::cookie::CookieManager;
    use libqaul::{
        Qaul,
        Identity,
    };
    use iron::{
        method::Method,
        middleware::BeforeMiddleware,
    };
    use super::*;

    fn setup() -> (RequestBuilder, Identity, QaulUserAuth, Authenticator) {
        let mut qaul = Qaul::start();
        let user_auth = qaul.user_create("a").unwrap();
        let qaul_core = QaulCore::new(&qaul);
        let (before_manager, _) = CookieManager::new();
        let mut rb = RequestBuilder::default_post();
        let auth = Authenticator::new();
        rb
            .add_middleware(QaulCore::new(&qaul))
            .add_middleware(before_manager)
            .add_middleware(JsonApi)
            .add_middleware(auth.clone());
        (rb, user_auth.clone().identity(), user_auth, auth)
    }

    #[test]
    fn valid_login_token() {
        let (mut rb, id, user_auth, auth) = setup();

        let go = rb
            .set_primary_data(UserAuth::from_identity(id.clone(), "a".into(), GrantType::Token).into())
            .request_response(|mut req| {
                let response = login(&mut req).unwrap();
                assert_eq!(req.extensions.get::<Cookies>().unwrap().get("bearer"), None);
                Ok(response)
            }).unwrap()
            .get_primary_data().unwrap();
        let ro : ResourceObject<UserGrant> = go.try_into().unwrap();
        let token = ro.id;
        assert_eq!(auth.tokens.lock().unwrap().get(&token), Some(&id));
    }

    #[test]
    fn valid_login_cookie() {
        let (mut rb, id, user_auth, auth) = setup();

        let go = rb
            .set_primary_data(UserAuth::from_identity(id.clone(), "a".into(), GrantType::Cookie).into())
            .request_response(|mut req| {
                let response = login(&mut req).unwrap();
                let token = req.extensions.get::<Cookies>().unwrap().get("bearer").unwrap();
                assert_eq!(auth.tokens.lock().unwrap().get(token.value()), Some(&id));
                Ok(response)
            }).unwrap()
            .get_primary_data().unwrap();
        let ro : ResourceObject<Success> = go.try_into().unwrap();
    }

    #[test]
    fn multiple_data() {
        let (mut rb, id, user_auth, _) = setup();

        rb.set_document(
                &Document { 
                    data: OptionalVec::Many(vec![]),
                    ..Default::default()
                })
            .request(|mut req| {
                assert!(login(&mut req).is_err())
            })
    }

    #[test]
    fn no_data() {
        let (mut rb, id, user_auth, _) = setup();

        rb.set_document(
                &Document { 
                    data: OptionalVec::NotPresent,
                    ..Default::default()
                })
            .request(|mut req| {
                assert!(login(&mut req).is_err())
            })
    }

    #[test]
    fn wrong_object() {
        let (mut rb, id, user_auth, _) = setup();

        rb.set_primary_data(Success::from_message("test".into()).into())
            .request(|mut req| {
                assert!(login(&mut req).is_err())
            })
    }

    #[test]
    fn invalid_identity() {
        let (mut rb, id, user_auth, _) = setup();

        rb.set_primary_data(ResourceObject::<UserAuth>::new("".into(), None).into())
            .request(|mut req| {
                assert!(login(&mut req).is_err())
            })
    }

    #[test]
    fn no_secret() {
        let (mut rb, id, user_auth, _) = setup();

        let mut ro = UserAuth::from_identity(id, "".into(), GrantType::Token);
        ro.attributes = None;
        rb.set_primary_data(ro.into())
            .request(|mut req| {
                assert!(login(&mut req).is_err())
            })
    }

    #[test]
    fn bad_password() {
        let (mut rb, id, user_auth, _) = setup();

        rb.set_primary_data(UserAuth::from_identity(id, "".into(), GrantType::Token).into())
            .request(|mut req| {
                assert!(login(&mut req).is_err())
            })
    }
}
