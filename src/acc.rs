//
use std::sync::Arc;

use crate::api::{ApiAccount, ApiIdentifier, ApiOrder};
use crate::cert::Certificate;
use crate::jwt::make_jws_kid;
use crate::order::{NewOrder, Order};
use crate::persist::{Persist, PersistKey, PersistKind};
use crate::util::{expect_header, read_json, retry_call, AcmeKey};
use crate::{Directory, Result};

#[derive(Clone)]
pub(crate) struct AccountInner<P: Persist> {
    pub directory: Directory<P>,
    pub contact_email: String,
    pub acme_key: AcmeKey,
    pub api_account: ApiAccount,
}

/// Account with an ACME provider.
///
/// Accounts are created using [`Directory::account`] and consist of a contact
/// email address and a private key for signing requests to the ACME API.
///
/// acme-lib uses elliptic curve P-256 for accessing the account. This
/// does not affect which key algorithms that can be used for the
/// issued certificates.
///
/// The advantage of using elliptic curve cryptography is that the signed
/// requests against the ACME lib are kept small and that the public key
/// can be derived from the private.
///
/// [`Directory::account`]: struct.Directory.html#method.account
#[derive(Clone)]
pub struct Account<P: Persist> {
    inner: Arc<AccountInner<P>>,
}

impl<P: Persist> Account<P> {
    pub(crate) fn new(
        directory: Directory<P>,
        contact_email: &str,
        acme_key: AcmeKey,
        api_account: ApiAccount,
    ) -> Self {
        Account {
            inner: Arc::new(AccountInner {
                directory,
                acme_key,
                contact_email: contact_email.into(),
                api_account,
            }),
        }
    }

    /// Private key for this account.
    ///
    /// The key is an elliptic curve private key.
    pub fn acme_private_key_pem(&self) -> String {
        String::from_utf8(self.inner.acme_key.to_pem()).expect("from_utf8")
    }

    /// Contact email for this account.
    pub fn contact_email(&self) -> &str {
        &self.inner.contact_email
    }

    /// Get an already issued and [downloaded] certificate.
    ///
    /// Every time a certificate is downloaded, the certificate and corresponding
    /// private key are persisted. This method returns an already existing certificate
    /// from the local storage (no API calls involved).
    ///
    /// This can form the basis for implemeting automatic renewal of
    /// certificates where the [valid days left] are running low.
    ///
    /// [downloaded]: order/struct.CertOrder.html#method.download_and_save_cert
    /// [valid days left]: struct.Certificate.html#method.valid_days_left
    pub fn certificate(&self, primary_name: &str) -> Result<Option<Certificate>> {
        // details needed for persistence
        let realm = &self.inner.contact_email;
        let persist = &self.inner.directory.persist();

        // read primary key
        let pk_key = PersistKey::new(realm, PersistKind::PrivateKey, primary_name);
        debug!("Read private key: {}", pk_key);
        let private_key = persist
            .get(&pk_key)?
            .and_then(|s| String::from_utf8(s).ok());

        // read certificate
        let pk_crt = PersistKey::new(realm, PersistKind::Certificate, primary_name);
        debug!("Read certificate: {}", pk_crt);
        let certificate = persist
            .get(&pk_crt)?
            .and_then(|s| String::from_utf8(s).ok());

        Ok(match (private_key, certificate) {
            (Some(k), Some(c)) => Some(Certificate::new(k, c)),
            _ => None,
        })
    }

    /// Create a new order to issue a certificate for this account.
    ///
    /// Each order has a required `primary_name` (which will be set as the certificates `CN`)
    /// and a variable number of `alt_names`.
    ///
    /// This library doesn't constrain the number of `alt_names`, but it is limited by the ACME
    /// API provider. Let's Encrypt sets a max of [100 names] per certificate.
    ///
    /// Every call creates a new order with the ACME API provider, even when the domain
    /// names supplied are exactly the same.
    ///
    /// [100 names]: https://letsencrypt.org/docs/rate-limits/
    pub fn new_order(&self, primary_name: &str, alt_names: &[&str]) -> Result<NewOrder<P>> {
        // construct the identifiers
        let prim_arr = [primary_name];
        let domains = prim_arr.iter().chain(alt_names);
        let order = ApiOrder {
            identifiers: domains
                .map(|s| ApiIdentifier {
                    _type: "dns".into(),
                    value: s.to_string(),
                })
                .collect(),
            ..Default::default()
        };

        let res = retry_call(|| {
            let nonce = self.inner.directory.new_nonce()?;
            let url = &self.inner.directory.api_directory().newOrder;
            let body = make_jws_kid(url, nonce, &self.inner.acme_key, &order)?;
            debug!("Call new order endpoint: {}", url);
            let mut req = ureq::post(url);
            req.set("content-type", "application/jose+json");
            Ok((req, Some(body)))
        })?;
        let url = expect_header(&res, "location")?;
        let api_order: ApiOrder = read_json(res)?;

        let order = Order::new(&self.inner, api_order, url);
        Ok(NewOrder { order })
    }

    /// Access the underlying JSON object for debugging.
    pub fn api_account(&self) -> &ApiAccount {
        &self.inner.api_account
    }
}

#[cfg(test)]
mod test {
    use crate::persist::*;
    use crate::*;

    #[test]
    fn test_create_order() -> Result<()> {
        let server = crate::test::with_directory_server();
        let url = DirectoryUrl::Other(&server.dir_url);
        let persist = MemoryPersist::new();
        let dir = Directory::from_url(persist, url)?;
        let acc = dir.account("foo@bar.com")?;
        let _ = acc.new_order("acmetest.example.com", &[])?;
        Ok(())
    }
}
