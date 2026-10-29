//! A small typed client for the Paddle Billing API calls a hosted hub makes.
//!
//! The client creates a checkout, changes a subscription's items, opens the
//! customer portal and makes sure the five-price catalog exists. Every request
//! carries `Authorization: Bearer <key>` and `Paddle-Version: 1`. Nothing in
//! the hub calls it yet except `agmen-hub paddle-catalog`: the webhook handler
//! (hosted Task 6) and the billing routes (hosted Task 7) build on it.
//!
//! # What the Paddle docs say
//!
//! Read on 2026-10-29. The URLs below are the pages the shapes in this file
//! come from.
//!
//! - Authentication, <https://developer.paddle.com/api-reference/about/authentication>:
//!   "pass your Paddle API key using the `Authorization` header and the `Bearer`
//!   prefix."
//! - Versioning, <https://developer.paddle.com/api-reference/about/versioning>:
//!   "The current version of the Paddle API is **version 1**." Sending
//!   `Paddle-Version: 1` pins it, and the header "takes priority over your
//!   account's default version".
//! - Pagination, <https://developer.paddle.com/api-reference/about/pagination>:
//!   `per_page` defaults to 50 and is capped at 200, `after` is the id of the
//!   last entity on the previous page, and "check `has_more`" because `next` is
//!   returned even on the last page. The client reads only the `after` value of
//!   `next`, never its host, so a bearer key cannot be sent anywhere else.
//! - Errors, <https://developer.paddle.com/api-reference/about/errors>: a failed
//!   call answers `{"error": {"type", "code", "detail", ...}, "meta": ...}`.
//!   [`PaddleError::Refused`] keeps `error.code`.
//! - Custom data, <https://developer.paddle.com/api-reference/about/custom-data>
//!   and <https://developer.paddle.com/build/transactions/custom-data/>.
//!
//! ## The four questions hosted Tasks 6 and 7 are built on
//!
//! **(a) Does the tenant reach the subscription and the webhooks?** Yes, by the
//! docs. <https://developer.paddle.com/build/transactions/custom-data/>: "Where
//! a transaction has custom data against it, that data is copied to the created
//! subscription for your reference." Later transactions (renewals, upgrades) get
//! the subscription's custom data the same way ("Where a subscription has custom
//! data against it, that data is copied to any transactions created from it").
//! Every webhook entity has a `custom_data` field, on `subscription.*`
//! (<https://developer.paddle.com/webhooks/subscriptions/subscription-activated>)
//! and on `transaction.completed`
//! (<https://developer.paddle.com/webhooks/transactions/transaction-completed>:
//! "`data.custom_data`: Your key-value object, or null"). So
//! `data.custom_data.agmen_tenant` should be present on both. The docs' example
//! payloads show `custom_data: null` and no page promises the copy for a checkout
//! opened from a transaction link, so this is unverified until a sandbox
//! checkout runs. If a `subscription.*` event arrives without it, the hub finds
//! the tenant from the stored `paddle_subscription_id`
//! (`tenant_plans.paddle_subscription_id UNIQUE`, migration 0020), which the
//! first `transaction.completed` (it carries both `data.subscription_id` and the
//! tenant) writes.
//!
//! **(b) Which shape do items have?** Nested: `price.id` plus `quantity`, never
//! a flat `price_id`, in an update-subscription response
//! (<https://developer.paddle.com/api-reference/subscriptions/update-subscription>,
//! example `items[0].price.id` and `items[0].quantity`) and in every
//! subscription webhook (<https://developer.paddle.com/webhooks/subscriptions/subscription-created>:
//! "Each nests its price under a `price` object"). `transaction.completed`
//! (<https://developer.paddle.com/webhooks/transactions/transaction-completed>)
//! has `data.items[].price.id` and `quantity` too, while
//! `data.details.line_items[]` uses a flat `price_id`. Only a request uses
//! `price_id`: "Each catalog item requires `price_id`" and `quantity` (an integer
//! of at least 1) in the update-subscription and create-transaction pages.
//! Items also carry a `status` of `active`, `inactive` or `trialing`;
//! [`PaddleClient::update_items`] leaves out an `inactive` one. That is a
//! judgement: the page does not say removed items come back as `inactive`.
//!
//! **(c) Is `checkout.url` tied to the default payment link?** Yes.
//! <https://developer.paddle.com/api-reference/transactions/create-transaction>:
//! `checkout.url` is "Paddle Checkout URL for this transaction, composed of the
//! URL passed in the request or your default payment URL + `?_ptxn=` and the
//! Paddle ID for this transaction", and `checkout` is "Returned for
//! automatically-collected transactions and where `billing_details.enable_checkout`
//! is `true` for manually-collected transactions; `null` otherwise."
//! <https://developer.paddle.com/build/transactions/pass-transaction-checkout>
//! says you need to add a default payment link in the Paddle dashboard first.
//! The reference page does not say what the API answers when none is set, so
//! [`PaddleClient::create_checkout`] answers [`PaddleError::Shape`] naming the
//! default payment link when `checkout` or its `url` is null or missing. Setting
//! and approving that link is an owner action in the Paddle dashboard.
//!
//! **(d) Does the portal session need `subscription_ids`?** No.
//! <https://developer.paddle.com/api-reference/customer-portals/create-customer-portal-session/>:
//! `subscription_ids` is optional, "List of subscriptions to create
//! authenticated customer portal deep links for", at most 25 unique ids. The
//! response has `urls.general.overview` ("Link to the overview page in the
//! customer portal") and `urls.subscriptions[]`, which is "Empty if no
//! subscriptions passed in the request". [`PaddleClient::portal_url`] sends the
//! one subscription id (so the session also carries that subscription's deep
//! links, which this client does not read) and returns the overview link.
//!
//! ## Webhook shapes hosted Task 6 parses
//!
//! Pages under <https://developer.paddle.com/webhooks/>: `subscriptions/subscription-created`,
//! `subscription-activated`, `subscription-updated`, `subscription-past-due`,
//! `subscription-canceled` and `transactions/transaction-completed`. Each
//! notification is `{event_id ("evt_..."), event_type, occurred_at (RFC 3339),
//! notification_id ("ntf_..."), data}`, and `data` is the whole entity except a
//! subscription's `management_urls`. A subscription's `status` is one of
//! `active`, `canceled`, `past_due`, `paused` or `trialing`. The signature
//! header is `Paddle-Signature: ts=<unix>;h1=<hex>`, an HMAC-SHA256 over
//! `<ts>:<raw body>` with the destination's secret
//! (<https://developer.paddle.com/webhooks/signature-verification>), and more
//! than one `h1` may appear during secret rotation. The page says Paddle's SDKs
//! reject a timestamp "more than five seconds from the current time"; the plan's
//! 300 second window is looser. Delivery order is not guaranteed and events can
//! repeat (<https://developer.paddle.com/webhooks/about/respond-to-webhooks>).
//!
//! ## Products and prices
//!
//! - List products, <https://developer.paddle.com/api-reference/products/list-products>:
//!   `GET /products`, `status` defaults to `active`, response `data[]` and
//!   `meta.pagination {per_page, next, has_more, estimated_total}`.
//! - Create product, <https://developer.paddle.com/api-reference/products/create-product>:
//!   `name` and `tax_category` are required. [`PRODUCT_TAX_CATEGORY`] is `saas`:
//!   "Products that allow users to connect to and use online or cloud-based
//!   applications over the Internet." The other candidate, `standard`, is for
//!   "Software products that are pre-written and can be downloaded and installed
//!   onto a local device." The category must also be enabled on the Paddle
//!   account, which is the owner's check before the production run.
//! - List prices, <https://developer.paddle.com/api-reference/prices/list-prices>:
//!   `GET /prices`, `status` defaults to `active`, and `custom_data` is on each
//!   price. The list cannot filter on it, so the client filters.
//! - Create price, <https://developer.paddle.com/api-reference/prices/create-price>:
//!   required are `description` (2 to 500 characters), `product_id` and
//!   `unit_price {amount, currency_code}` (the amount is a string in the
//!   currency's smallest unit). `billing_cycle {interval, frequency}` makes it
//!   recurring. "If omitted, defaults to 1-100" for `quantity`. The page does
//!   not say whether a buyer can pick a quantity at checkout, so the four prices
//!   that are bought at quantity 1 leave `quantity` unset, as the plan says, and
//!   the seat price sets 1 to 1000.

use agmen_proto::TenantId;
use reqwest::Method;
use serde_json::{Value, json};
use url::Url;

use super::{Catalog, LineItem};
use crate::config::{PaddleConfig, Secret};

/// The API version every request pins. See the versioning page above.
const PADDLE_VERSION: &str = "1";

/// The tax category of both products.
///
/// `saas` fits software sold as a subscription to a hosted service. This is an
/// owner decision the plan leaves open: Andy confirms it against the categories
/// enabled on the Paddle account before the production catalog run.
pub const PRODUCT_TAX_CATEGORY: &str = "saas";

/// The most pages a list loop reads.
const MAX_PAGES: usize = 20;
/// The page size a list loop asks for, Paddle's maximum.
const PAGE_SIZE: usize = 200;
/// The most bytes of one response the client reads.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

const TEAM_PRODUCT: &str = "agmen Team";
const BUSINESS_PRODUCT: &str = "agmen Business";

/// Why a Paddle call failed. Its text never holds the API key.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PaddleError {
    /// The request could not be sent or its answer could not be read.
    #[error("the Paddle request failed")]
    Http(#[from] reqwest::Error),
    /// Paddle answered with an error status.
    #[error("Paddle refused the request with status {status} and code {code:?}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// Paddle's `error.code`, or an empty string when the body has none.
        code: String,
    },
    /// Paddle answered, but not in a shape this client can use.
    #[error("Paddle answered in an unexpected shape: {0}")]
    Shape(String),
}

fn shape(message: impl Into<String>) -> PaddleError {
    PaddleError::Shape(message.into())
}

/// One price the catalog needs.
struct PriceSpec {
    /// `custom_data.agmen_key`, which is also the [`Catalog`] field name.
    key: &'static str,
    product: &'static str,
    description: &'static str,
    /// Smallest currency unit, as Paddle wants it.
    amount: &'static str,
    /// Buyable quantity range, when the plan states one.
    quantity: Option<(u32, u32)>,
}

/// The five monthly USD prices, in [`Catalog`] field order.
const PRICES: [PriceSpec; 5] = [
    PriceSpec {
        key: "team_base",
        product: TEAM_PRODUCT,
        description: "agmen Team base",
        amount: "2500",
        quantity: None,
    },
    PriceSpec {
        key: "team_seat",
        product: TEAM_PRODUCT,
        description: "agmen Team extra seat",
        amount: "1000",
        quantity: Some((1, 1000)),
    },
    PriceSpec {
        key: "business_10",
        product: BUSINESS_PRODUCT,
        description: "agmen Business 10 seats",
        amount: "10000",
        quantity: None,
    },
    PriceSpec {
        key: "business_25",
        product: BUSINESS_PRODUCT,
        description: "agmen Business 25 seats",
        amount: "20000",
        quantity: None,
    },
    PriceSpec {
        key: "business_50",
        product: BUSINESS_PRODUCT,
        description: "agmen Business 50 seats",
        amount: "35000",
        quantity: None,
    },
];

/// A client for the Paddle Billing API. Cheap to clone.
#[derive(Debug, Clone)]
pub struct PaddleClient {
    http: reqwest::Client,
    base: Url,
    api_key: Secret,
}

impl PaddleClient {
    /// A client for `config.api_url`, sending `config.api_key` as a bearer token.
    ///
    /// The scheme is not checked here: [`crate::Config`] requires https for a
    /// real hub, and tests point the client at a loopback fake over http.
    ///
    /// # Errors
    ///
    /// [`PaddleError::Shape`] when the API URL cannot carry a path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = agmen_hub::PaddleConfig::api_from_env()?;
    /// let client = agmen_hub::PaddleClient::new(&config, reqwest::Client::new())?;
    /// # let _ = client;
    /// # Ok(()) }
    /// ```
    pub fn new(config: &PaddleConfig, http: reqwest::Client) -> Result<Self, PaddleError> {
        if config.api_url.cannot_be_a_base() {
            return Err(shape("the Paddle API URL cannot carry a path"));
        }
        Ok(Self {
            http,
            base: config.api_url.clone(),
            api_key: config.api_key.clone(),
        })
    }

    /// Starts a checkout for `tenant` and returns the page to send the buyer to.
    ///
    /// `POST /transactions` with `items` and `custom_data.agmen_tenant`, per
    /// <https://developer.paddle.com/api-reference/transactions/create-transaction>.
    /// The URL is the response's `data.checkout.url`.
    ///
    /// # Errors
    ///
    /// [`PaddleError::Http`] or [`PaddleError::Refused`] when the call fails, and
    /// [`PaddleError::Shape`] when `checkout.url` is missing, null or not a URL
    /// (the usual cause is no default payment link in the Paddle dashboard).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(client: agmen_hub::PaddleClient, tenant: agmen_proto::TenantId)
    /// #     -> Result<(), Box<dyn std::error::Error>> {
    /// let items = [agmen_hub::LineItem { price_id: "pri_example".to_owned(), quantity: 1 }];
    /// let checkout = client.create_checkout(&tenant, &items).await?;
    /// println!("{checkout}");
    /// # Ok(()) }
    /// ```
    pub async fn create_checkout(
        &self,
        tenant: &TenantId,
        items: &[LineItem],
    ) -> Result<Url, PaddleError> {
        let url = endpoint(&self.base, &["transactions"])?;
        let response = self
            .call(Method::POST, url, Some(&checkout_body(tenant, items)))
            .await?;
        checkout_url(&response)
    }

    /// Replaces a subscription's items and returns the items it now has.
    ///
    /// `PATCH /subscriptions/{id}` with `items` and `proration_billing_mode`
    /// `prorated_immediately`, per
    /// <https://developer.paddle.com/api-reference/subscriptions/update-subscription>.
    /// That page says to "send the complete list of items", and an item left out
    /// is removed, so `items` is the whole new list. An item with quantity 0 is
    /// left out of the request.
    ///
    /// # Errors
    ///
    /// [`PaddleError::Shape`] without sending anything when no item is left after
    /// dropping the zero quantities (Paddle would refuse an empty list), or when
    /// the response has no readable items. [`PaddleError::Http`] and
    /// [`PaddleError::Refused`] when the call fails.
    pub async fn update_items(
        &self,
        subscription_id: &str,
        items: &[LineItem],
    ) -> Result<Vec<LineItem>, PaddleError> {
        let body = update_body(items)?;
        let url = endpoint(&self.base, &["subscriptions", subscription_id])?;
        let response = self.call(Method::PATCH, url, Some(&body)).await?;
        items_of(&response)
    }

    /// A short-lived link to the customer portal for `customer_id`.
    ///
    /// `POST /customers/{id}/portal-sessions` with `subscription_ids`, per
    /// <https://developer.paddle.com/api-reference/customer-portals/create-customer-portal-session/>.
    /// The link is `data.urls.general.overview`. Paddle says to create a fresh
    /// session each time instead of keeping one.
    ///
    /// # Errors
    ///
    /// [`PaddleError::Http`] or [`PaddleError::Refused`] when the call fails, and
    /// [`PaddleError::Shape`] when the overview link is missing or not a URL.
    pub async fn portal_url(
        &self,
        customer_id: &str,
        subscription_id: &str,
    ) -> Result<Url, PaddleError> {
        let url = endpoint(&self.base, &["customers", customer_id, "portal-sessions"])?;
        let body = json!({"subscription_ids": [subscription_id]});
        let response = self.call(Method::POST, url, Some(&body)).await?;
        url_at(
            &response,
            "/data/urls/general/overview",
            "the portal overview link",
        )
    }

    /// Finds or creates the two products and five prices, and returns their ids.
    ///
    /// A product matches by exact `name` among active products, a price by
    /// `custom_data.agmen_key` among active prices. A second run creates
    /// nothing. A price Paddle already has for a key is never changed.
    ///
    /// # Errors
    ///
    /// [`PaddleError::Http`] or [`PaddleError::Refused`] when a call fails, and
    /// [`PaddleError::Shape`] when a list is not readable or runs past 20 pages
    /// of 200.
    pub async fn ensure_catalog(&self) -> Result<Catalog, PaddleError> {
        let products = self.list("products").await?;
        let mut product_ids = Vec::with_capacity(2);
        for name in [TEAM_PRODUCT, BUSINESS_PRODUCT] {
            let found = products
                .iter()
                .filter(|row| is_active(row))
                .find(|row| row.get("name").and_then(Value::as_str) == Some(name))
                .and_then(id_of);
            let id = match found {
                Some(id) => id,
                None => self.create_product(name).await?,
            };
            product_ids.push((name, id));
        }
        let prices = self.list("prices").await?;
        let mut price_ids = Vec::with_capacity(PRICES.len());
        for spec in &PRICES {
            let found = prices
                .iter()
                .filter(|row| is_active(row))
                .find(|row| {
                    row.pointer("/custom_data/agmen_key")
                        .and_then(Value::as_str)
                        == Some(spec.key)
                })
                .and_then(id_of);
            let id = match found {
                Some(id) => id,
                None => {
                    let product_id = product_ids
                        .iter()
                        .find_map(|(name, id)| (*name == spec.product).then_some(id.as_str()))
                        .ok_or_else(|| shape("a price names a product the catalog lacks"))?;
                    self.create_price(spec, product_id).await?
                }
            };
            price_ids.push(id);
        }
        let mut ids = price_ids.into_iter();
        let mut next = || ids.next().ok_or_else(|| shape("the catalog lacks a price"));
        Ok(Catalog {
            team_base: next()?,
            team_seat: next()?,
            business_10: next()?,
            business_25: next()?,
            business_50: next()?,
        })
    }

    async fn create_product(&self, name: &str) -> Result<String, PaddleError> {
        let body = json!({"name": name, "tax_category": PRODUCT_TAX_CATEGORY});
        let url = endpoint(&self.base, &["products"])?;
        let created = self.call(Method::POST, url, Some(&body)).await?;
        let id = created
            .get("data")
            .and_then(id_of)
            .ok_or_else(|| shape("the created product has no id"))?;
        tracing::info!(product = name, id = %id, "created a Paddle product");
        Ok(id)
    }

    async fn create_price(
        &self,
        spec: &PriceSpec,
        product_id: &str,
    ) -> Result<String, PaddleError> {
        let url = endpoint(&self.base, &["prices"])?;
        let created = self
            .call(Method::POST, url, Some(&price_body(spec, product_id)))
            .await?;
        let id = created
            .get("data")
            .and_then(id_of)
            .ok_or_else(|| shape("the created price has no id"))?;
        tracing::info!(price = spec.key, id = %id, "created a Paddle price");
        Ok(id)
    }

    /// Every active row of `GET /{endpoint}`, at most [`MAX_PAGES`] pages of
    /// [`PAGE_SIZE`].
    async fn list(&self, endpoint_name: &str) -> Result<Vec<Value>, PaddleError> {
        let mut rows = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = endpoint(&self.base, &[endpoint_name])?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("per_page", &PAGE_SIZE.to_string());
                if let Some(cursor) = &after {
                    query.append_pair("after", cursor);
                }
            }
            let page = self.call(Method::GET, url, None).await?;
            let data = page
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| shape(format!("GET /{endpoint_name} has no data list")))?;
            rows.extend(data.iter().cloned());
            match next_cursor(&page, endpoint_name)? {
                None => return Ok(rows),
                Some(cursor) if after.as_deref() == Some(cursor.as_str()) => {
                    return Err(shape(format!(
                        "GET /{endpoint_name} repeats its cursor instead of advancing"
                    )));
                }
                Some(cursor) => after = Some(cursor),
            }
        }
        Err(shape(format!(
            "GET /{endpoint_name} has more than {MAX_PAGES} pages of {PAGE_SIZE}"
        )))
    }

    /// Sends one request and returns the JSON body of a success.
    async fn call(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> Result<Value, PaddleError> {
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(self.api_key.expose())
            .header("Paddle-Version", PADDLE_VERSION);
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request.send().await?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(shape(format!(
                    "the answer is longer than {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(PaddleError::Refused {
                status: status.as_u16(),
                code: refusal_code(&bytes),
            });
        }
        serde_json::from_slice(&bytes).map_err(|_| shape("the answer is not JSON"))
    }
}

/// Makes sure the Paddle catalog exists and returns its price ids.
///
/// This is `agmen-hub paddle-catalog`: it builds the hub's upstream client for
/// `config` and runs [`PaddleClient::ensure_catalog`]. Only `api_url` and
/// `api_key` are used.
///
/// # Errors
///
/// [`PaddleError`] as for [`PaddleClient::ensure_catalog`], or
/// [`PaddleError::Http`] when the HTTP client cannot be built.
///
/// # Examples
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config = agmen_hub::PaddleConfig::api_from_env()?;
/// let catalog = agmen_hub::sync_catalog(&config).await?;
/// print!("{}", catalog.env_lines());
/// # Ok(()) }
/// ```
pub async fn sync_catalog(config: &PaddleConfig) -> Result<Catalog, PaddleError> {
    let http = crate::http::upstream_client()?;
    PaddleClient::new(config, http)?.ensure_catalog().await
}

/// `base` with `segments` appended to its path, each as one encoded segment.
fn endpoint(base: &Url, segments: &[&str]) -> Result<Url, PaddleError> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|()| shape("the Paddle API URL cannot carry a path"))?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

fn line_item_json(item: &LineItem) -> Value {
    json!({"price_id": item.price_id, "quantity": item.quantity})
}

fn checkout_body(tenant: &TenantId, items: &[LineItem]) -> Value {
    json!({
        "items": items.iter().map(line_item_json).collect::<Vec<_>>(),
        "custom_data": {"agmen_tenant": tenant.to_string()},
    })
}

fn update_body(items: &[LineItem]) -> Result<Value, PaddleError> {
    let kept: Vec<Value> = items
        .iter()
        .filter(|item| item.quantity > 0)
        .map(line_item_json)
        .collect();
    if kept.is_empty() {
        return Err(shape(
            "a subscription needs at least one item with a quantity above 0",
        ));
    }
    Ok(json!({"items": kept, "proration_billing_mode": "prorated_immediately"}))
}

fn price_body(spec: &PriceSpec, product_id: &str) -> Value {
    let mut body = json!({
        "description": spec.description,
        "product_id": product_id,
        "unit_price": {"amount": spec.amount, "currency_code": "USD"},
        "billing_cycle": {"interval": "month", "frequency": 1},
        "custom_data": {"agmen_key": spec.key},
    });
    if let (Some((minimum, maximum)), Some(object)) = (spec.quantity, body.as_object_mut()) {
        object.insert(
            "quantity".to_owned(),
            json!({"minimum": minimum, "maximum": maximum}),
        );
    }
    body
}

/// A URL at JSON pointer `pointer`, or [`PaddleError::Shape`] naming `what`.
fn url_at(response: &Value, pointer: &str, what: &str) -> Result<Url, PaddleError> {
    let text = response
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| shape(format!("{what} is missing")))?;
    Url::parse(text).map_err(|_| shape(format!("{what} is not a URL")))
}

fn checkout_url(response: &Value) -> Result<Url, PaddleError> {
    if response
        .pointer("/data/checkout/url")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(shape(
            "Paddle returned no checkout URL; set and approve a default payment link \
             in the Paddle dashboard (Checkout settings)",
        ));
    }
    url_at(response, "/data/checkout/url", "the checkout URL")
}

/// The items of a subscription response, as the plan's line items.
fn items_of(response: &Value) -> Result<Vec<LineItem>, PaddleError> {
    entity_items(&response["data"])
}

/// The items of a Paddle subscription entity, including webhook data.
pub(super) fn entity_items(entity: &Value) -> Result<Vec<LineItem>, PaddleError> {
    let items = entity
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| shape("the subscription has no items list"))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if item.get("status").and_then(Value::as_str) == Some("inactive") {
            continue;
        }
        let price_id = item
            .pointer("/price/id")
            .and_then(Value::as_str)
            .ok_or_else(|| shape("a subscription item has no price id"))?;
        let quantity = item
            .get("quantity")
            .and_then(Value::as_u64)
            .and_then(|quantity| u32::try_from(quantity).ok())
            .ok_or_else(|| shape("a subscription item has no usable quantity"))?;
        out.push(LineItem {
            price_id: price_id.to_owned(),
            quantity,
        });
    }
    Ok(out)
}

fn id_of(row: &Value) -> Option<String> {
    row.get("id").and_then(Value::as_str).map(str::to_owned)
}

/// A row is active unless its `status` says otherwise.
fn is_active(row: &Value) -> bool {
    row.get("status")
        .and_then(Value::as_str)
        .is_none_or(|status| status == "active")
}

/// Paddle's `error.code` in an error body, or an empty string.
fn refusal_code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

/// The `after` cursor of the next page, or `None` on the last page.
///
/// Only the `after` query value of `meta.pagination.next` is used, never the
/// URL itself.
fn next_cursor(page: &Value, endpoint_name: &str) -> Result<Option<String>, PaddleError> {
    let pagination = page.pointer("/meta/pagination");
    let has_more = pagination
        .and_then(|p| p.get("has_more"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !has_more {
        return Ok(None);
    }
    pagination
        .and_then(|p| p.get("next"))
        .and_then(Value::as_str)
        .and_then(|next| Url::parse(next).ok())
        .and_then(|next| {
            next.query_pairs()
                .find_map(|(key, value)| (key == "after").then(|| value.into_owned()))
        })
        .map(Some)
        .ok_or_else(|| shape(format!("GET /{endpoint_name} has more pages but no cursor")))
}

#[cfg(test)]
#[path = "paddle_tests.rs"]
mod tests;
