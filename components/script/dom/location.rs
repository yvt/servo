/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use crate::dom::bindings::codegen::Bindings::LocationBinding::LocationMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowBinding::WindowMethods;
use crate::dom::bindings::error::{Error, ErrorResult, Fallible};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::reflector::{reflect_dom_object, Reflector};
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::bindings::str::USVString;
use crate::dom::dissimilaroriginwindow::DissimilarOriginWindow;
use crate::dom::document::Document;
use crate::dom::globalscope::GlobalScope;
use crate::dom::urlhelper::UrlHelper;
use crate::dom::window::Window;
use dom_struct::dom_struct;
use net_traits::request::Referrer;
use script_traits::{HistoryEntryReplacement, LoadData, LoadOrigin};
use servo_url::ServoUrl;

#[derive(PartialEq)]
enum NavigationType {
    /// The "[`Location`-object navigate][1]" steps.
    ///
    /// [1]: https://html.spec.whatwg.org/multipage/#location-object-navigate
    Normal,

    /// The last step of [`reload()`][1] (`reload_triggered == true`)
    ///
    /// [1]: https://html.spec.whatwg.org/multipage/#dom-location-reload
    ReloadByScript,

    /// User-requested navigation (the unlabeled paragraph after
    /// [`reload()`][1]).
    ///
    /// [1]: https://html.spec.whatwg.org/multipage/#dom-location-reload
    ReloadByConstellation,
}

#[dom_struct]
pub struct Location {
    reflector_: Reflector,
    /// This `Location`'s relevant global object from script code's point of
    /// view. `None` if it's cross-site.
    ///
    /// If it's `None`, pretend like the relevant `Document` has already been
    /// discarded.
    window: Option<Dom<Window>>,
}

impl Location {
    fn new_inherited(window: Option<&Window>) -> Location {
        Location {
            reflector_: Reflector::new(),
            window: window.map(Dom::from_ref),
        }
    }

    pub fn new(window: &Window) -> DomRoot<Location> {
        reflect_dom_object(Box::new(Location::new_inherited(Some(window))), window)
    }

    /// Construct a `Location` object for a remote document.
    ///
    /// This function essentially creates a proxy to a real `Location` object
    /// living somewhere outside the current script thread.
    pub fn new_remote(window: &DissimilarOriginWindow) -> DomRoot<Location> {
        reflect_dom_object(Box::new(Location::new_inherited(None)), window)
    }

    /// Navigate the relevant `Document`'s browsing context.
    fn navigate(
        &self,
        url: ServoUrl,
        replacement_flag: HistoryEntryReplacement,
        ty: NavigationType,
    ) {
        let incumbent_global;

        let window = if let Some(window) = &self.window {
            &**window
        } else {
            return;
        };

        // The active document of the source browsing context used for
        // navigation determines the request's referrer and referrer policy.
        let source_window = match ty {
            NavigationType::ReloadByScript | NavigationType::ReloadByConstellation => {
                // > Navigate the browsing context [...] the source browsing context
                // > set to the browsing context being navigated.
                window
            },
            NavigationType::Normal => {
                // > 2. Let `sourceBrowsingContext` be the incumbent global object's
                // >    browsing context.
                incumbent_global = GlobalScope::incumbent().expect("no incumbent global object");
                incumbent_global
                    .downcast::<Window>()
                    .expect("global object is not a Window")
            },
        };
        let source_document = source_window.Document();

        let referrer = Referrer::ReferrerUrl(source_document.url());
        let referrer_policy = source_document.get_referrer_policy();

        // <https://html.spec.whatwg.org/multipage/#navigate>
        // > Let `incumbentNavigationOrigin` be the origin of the incumbent
        // > settings object, or if no script was involved, [...]
        let (load_origin, creator_pipeline_id) = match ty {
            NavigationType::Normal | NavigationType::ReloadByScript => {
                let incumbent_global =
                    GlobalScope::incumbent().expect("no incumbent global object");
                let incumbent_window = incumbent_global
                    .downcast::<Window>()
                    .expect("global object is not a Window");
                (
                    LoadOrigin::Script(incumbent_window.origin().immutable().clone()),
                    Some(incumbent_window.pipeline_id()),
                )
            },
            NavigationType::ReloadByConstellation => (LoadOrigin::Constellation, None),
        };

        let reload_triggered = match ty {
            NavigationType::ReloadByScript | NavigationType::ReloadByConstellation => true,
            NavigationType::Normal => false,
        };

        let load_data = LoadData::new(
            load_origin,
            url,
            creator_pipeline_id,
            referrer,
            referrer_policy,
            None, // Top navigation doesn't inherit secure context
        );
        // TODO: rethrow exceptions, set exceptions enabled flag.
        window.load_url(replacement_flag, reload_triggered, load_data);
    }

    /// Get if this `Location`'s [relevant `Document`][1] is non-null.
    ///
    /// [1]: https://html.spec.whatwg.org/multipage/#relevant-document
    fn has_document(&self) -> bool {
        // <https://html.spec.whatwg.org/multipage/#relevant-document>
        //
        // > A `Location` object has an associated relevant `Document`, which is
        // > this `Location` object's relevant global object's browsing
        // > context's active document, if this `Location` object's relevant
        // > global object's browsing context is non-null, and null otherwise.
        self.window
            .as_ref()
            .map_or(false, |w| w.Document().browsing_context().is_some())
    }

    /// Get this `Location` object's [relevant `Document`][1], or
    /// `Err(Error::Security)` if it's non-null and its origin is not same
    /// origin-domain with the entry setting object's origin.
    ///
    /// In the specification's terms:
    ///
    ///  1. If this `Location` object's relevant `Document` is null, then return
    ///     null.
    ///
    ///  2. If this `Location` object's relevant `Document`'s origin is not same
    ///     origin-domain with the entry settings object's origin, then throw a
    ///     "`SecurityError`" `DOMException`.
    ///
    ///  3. Return this `Location` object's relevant `Document`.
    ///
    /// [1]: https://html.spec.whatwg.org/multipage/#relevant-document
    fn document_if_same_origin(&self) -> Fallible<Option<DomRoot<Document>>> {
        // <https://html.spec.whatwg.org/multipage/#relevant-document>
        //
        // > A `Location` object has an associated relevant `Document`, which is
        // > this `Location` object's relevant global object's browsing
        // > context's active document, if this `Location` object's relevant
        // > global object's browsing context is non-null, and null otherwise.
        if let Some(window_proxy) = self
            .window
            .as_ref()
            .and_then(|w| w.Document().browsing_context())
        {
            // `Location`'s many other operations:
            //
            // > If this `Location` object's relevant `Document` is non-null and
            // > its origin is not same origin-domain with the entry settings
            // > object's origin, then throw a "SecurityError" `DOMException`.
            //
            // FIXME: We should still return the active document if it's same
            //        origin but not fully active. `WindowProxy::document`
            //        currently returns `None` in this case.
            if let Some(document) = window_proxy.document().filter(|document| {
                self.entry_settings_object()
                    .origin()
                    .same_origin_domain(document.origin())
            }) {
                Ok(Some(document))
            } else {
                Err(Error::Security)
            }
        } else {
            // The browsing context is null
            Ok(None)
        }
    }

    /// Get this `Location` object's [relevant url][1] or
    /// `Err(Error::Security)` if the [relevant `Document`][2] if it's non-null
    /// and its origin is not same origin-domain with the entry setting object's
    /// origin.
    ///
    /// [1]: https://html.spec.whatwg.org/multipage/#concept-location-url
    /// [2]: https://html.spec.whatwg.org/multipage/#relevant-document
    fn get_url_if_same_origin(&self) -> Fallible<ServoUrl> {
        Ok(if let Some(document) = self.document_if_same_origin()? {
            document.url()
        } else {
            ServoUrl::parse("about:blank").unwrap()
        })
    }

    fn entry_settings_object(&self) -> DomRoot<GlobalScope> {
        GlobalScope::entry()
    }

    /// The common algorithm for `Location`'s setters and `Location::Assign`.
    #[inline]
    fn setter_common(&self, f: impl FnOnce(ServoUrl) -> Fallible<Option<ServoUrl>>) -> ErrorResult {
        // Step 1: If this Location object's relevant Document is null, then return.
        // Step 2: If this Location object's relevant Document's origin is not
        // same origin-domain with the entry settings object's origin, then
        // throw a "SecurityError" DOMException.
        if let Some(document) = self.document_if_same_origin()? {
            // Step 3: Let copyURL be a copy of this Location object's url.
            // Step 4: Assign the result of running f(copyURL) to copyURL.
            if let Some(copy_url) = f(document.url())? {
                // Step 5: Terminate these steps if copyURL is null.
                // Step 6: Location-object navigate to copyURL.
                self.navigate(
                    copy_url,
                    HistoryEntryReplacement::Disabled,
                    NavigationType::Normal,
                );
            }
        }
        Ok(())
    }

    /// Perform a user-requested reload (the unlabeled paragraph after
    /// [`reload()`][1]).
    ///
    /// This method mustn't be called for a `Location` object created by
    /// [`Self::new_remote`].
    ///
    /// [1]: https://html.spec.whatwg.org/multipage/#dom-location-reload
    pub fn reload_without_origin_check(&self) {
        // > When a user requests that the active document of a browsing context
        // > be reloaded through a user interface element, the user agent should
        // > navigate the browsing context to the same resource as that
        // > `Document`, with `historyHandling` set to "reload".
        let url = self
            .window
            .as_ref()
            .expect("this operation is invalid for a `Location` proxy")
            .get_url();
        self.navigate(
            url,
            HistoryEntryReplacement::Enabled,
            NavigationType::ReloadByConstellation,
        );
    }
}

impl LocationMethods for Location {
    // https://html.spec.whatwg.org/multipage/#dom-location-assign
    fn Assign(&self, url: USVString) -> ErrorResult {
        self.setter_common(|_copy_url| {
            // Step 3: Parse url relative to the entry settings object. If that failed,
            // throw a "SyntaxError" DOMException.
            let base_url = self.entry_settings_object().api_base_url();
            let url = match base_url.join(&url.0) {
                Ok(url) => url,
                Err(_) => return Err(Error::Syntax),
            };

            Ok(Some(url))
        })
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-reload
    fn Reload(&self) -> ErrorResult {
        let url = self.get_url_if_same_origin()?;
        self.navigate(
            url,
            HistoryEntryReplacement::Enabled,
            NavigationType::ReloadByScript,
        );
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-replace
    fn Replace(&self, url: USVString) -> ErrorResult {
        // Step 1: If this Location object's relevant Document is null, then return.
        if self.has_document() {
            // Step 2: Parse url relative to the entry settings object. If that failed,
            // throw a "SyntaxError" DOMException.
            let base_url = self.entry_settings_object().api_base_url();
            let url = match base_url.join(&url.0) {
                Ok(url) => url,
                Err(_) => return Err(Error::Syntax),
            };
            // Step 3: Location-object navigate to the resulting URL record with
            // the replacement flag set.
            self.navigate(
                url,
                HistoryEntryReplacement::Enabled,
                NavigationType::Normal,
            );
        }
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-hash
    fn GetHash(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Hash(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-hash
    fn SetHash(&self, value: USVString) -> ErrorResult {
        self.setter_common(|mut copy_url| {
            // Step 4: Let input be the given value with a single leading "#" removed, if any.
            // Step 5: Set copyURL's fragment to the empty string.
            // Step 6: Basic URL parse input, with copyURL as url and fragment state as
            // state override.
            copy_url.as_mut_url().set_fragment(match value.0.as_str() {
                "" => Some("#"),
                _ if value.0.starts_with('#') => Some(&value.0[1..]),
                _ => Some(&value.0),
            });

            Ok(Some(copy_url))
        })
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-host
    fn GetHost(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Host(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-host
    fn SetHost(&self, value: USVString) -> ErrorResult {
        self.setter_common(|mut copy_url| {
            // Step 4: If copyURL's cannot-be-a-base-URL flag is set, terminate these steps.
            if copy_url.cannot_be_a_base() {
                return Ok(None);
            }

            // Step 5: Basic URL parse the given value, with copyURL as url and host state
            // as state override.
            let _ = copy_url.as_mut_url().set_host(Some(&value.0));

            Ok(Some(copy_url))
        })
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-origin
    fn GetOrigin(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Origin(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-hostname
    fn GetHostname(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Hostname(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-hostname
    fn SetHostname(&self, value: USVString) -> ErrorResult {
        self.setter_common(|mut copy_url| {
            // Step 4: If copyURL's cannot-be-a-base-URL flag is set, terminate these steps.
            if copy_url.cannot_be_a_base() {
                return Ok(None);
            }

            // Step 5: Basic URL parse the given value, with copyURL as url and hostname
            // state as state override.
            let _ = copy_url.as_mut_url().set_host(Some(&value.0));

            Ok(Some(copy_url))
        })
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-href
    fn GetHref(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Href(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-href
    fn SetHref(&self, value: USVString) -> ErrorResult {
        // Step 1: If this Location object's relevant Document is null, then return.
        if self.has_document() {
            // Note: no call to self.check_same_origin_domain()
            // Step 2: Parse the given value relative to the entry settings object.
            // If that failed, throw a TypeError exception.
            let base_url = self.entry_settings_object().api_base_url();
            let url = match base_url.join(&value.0) {
                Ok(url) => url,
                Err(e) => return Err(Error::Type(format!("Couldn't parse URL: {}", e))),
            };
            // Step 3: Location-object navigate to the resulting URL record.
            self.navigate(
                url,
                HistoryEntryReplacement::Disabled,
                NavigationType::Normal,
            );
        }
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-pathname
    fn GetPathname(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Pathname(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-pathname
    fn SetPathname(&self, value: USVString) -> ErrorResult {
        self.setter_common(|mut copy_url| {
            // Step 4: If copyURL's cannot-be-a-base-URL flag is set, terminate these steps.
            if copy_url.cannot_be_a_base() {
                return Ok(None);
            }

            // Step 5: Set copyURL's path to the empty list.
            // Step 6: Basic URL parse the given value, with copyURL as url and path
            // start state as state override.
            copy_url.as_mut_url().set_path(&value.0);

            Ok(Some(copy_url))
        })
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-port
    fn GetPort(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Port(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-port
    fn SetPort(&self, value: USVString) -> ErrorResult {
        self.setter_common(|mut copy_url| {
            // Step 4: If copyURL cannot have a username/password/port, then return.
            // https://url.spec.whatwg.org/#cannot-have-a-username-password-port
            if copy_url.host().is_none() ||
                copy_url.cannot_be_a_base() ||
                copy_url.scheme() == "file"
            {
                return Ok(None);
            }

            // Step 5: If the given value is the empty string, then set copyURL's
            // port to null.
            // Step 6: Otherwise, basic URL parse the given value, with copyURL as url
            // and port state as state override.
            let _ = url::quirks::set_port(copy_url.as_mut_url(), &value.0);

            Ok(Some(copy_url))
        })
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-protocol
    fn GetProtocol(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Protocol(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-protocol
    fn SetProtocol(&self, value: USVString) -> ErrorResult {
        self.setter_common(|mut copy_url| {
            // Step 4: Let possibleFailure be the result of basic URL parsing the given
            // value, followed by ":", with copyURL as url and scheme start state as
            // state override.
            let scheme = match value.0.find(':') {
                Some(position) => &value.0[..position],
                None => &value.0,
            };

            if let Err(_) = copy_url.as_mut_url().set_scheme(scheme) {
                // Step 5: If possibleFailure is failure, then throw a "SyntaxError" DOMException.
                return Err(Error::Syntax);
            }

            // Step 6: If copyURL's scheme is not an HTTP(S) scheme, then terminate these steps.
            if !copy_url.scheme().eq_ignore_ascii_case("http") &&
                !copy_url.scheme().eq_ignore_ascii_case("https")
            {
                return Ok(None);
            }

            Ok(Some(copy_url))
        })
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-search
    fn GetSearch(&self) -> Fallible<USVString> {
        Ok(UrlHelper::Search(&self.get_url_if_same_origin()?))
    }

    // https://html.spec.whatwg.org/multipage/#dom-location-search
    fn SetSearch(&self, value: USVString) -> ErrorResult {
        self.setter_common(|mut copy_url| {
            // Step 4: If the given value is the empty string, set copyURL's query to null.
            // Step 5: Otherwise, run these substeps:
            //   1. Let input be the given value with a single leading "?" removed, if any.
            //   2. Set copyURL's query to the empty string.
            //   3. Basic URL parse input, with copyURL as url and query state as state
            //      override, and the relevant Document's document's character encoding as
            //      encoding override.
            copy_url.as_mut_url().set_query(match value.0.as_str() {
                "" => None,
                _ if value.0.starts_with('?') => Some(&value.0[1..]),
                _ => Some(&value.0),
            });

            Ok(Some(copy_url))
        })
    }
}
