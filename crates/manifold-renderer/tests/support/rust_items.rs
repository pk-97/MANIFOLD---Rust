/// Read the production declaration without counting the testkit variant twice.
struct TestkitVisible(syn::Item);

impl syn::parse::Parse for TestkitVisible {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        if input.peek(syn::Ident) && input.fork().parse::<syn::Ident>()? == "testkit" {
            let _: syn::Ident = input.parse()?;
            let testkit;
            syn::braced!(testkit in input);
            let _: syn::Item = testkit.parse()?;
            if !testkit.is_empty() {
                return Err(testkit.error("expected one testkit item"));
            }
            let keyword: syn::Ident = input.parse()?;
            if keyword != "production" {
                return Err(syn::Error::new(keyword.span(), "expected production"));
            }
            let production;
            syn::braced!(production in input);
            let item = production.parse()?;
            if !production.is_empty() {
                return Err(production.error("expected one production item"));
            }
            Ok(Self(item))
        } else {
            input.parse().map(Self)
        }
    }
}

pub fn test_only(attrs: &[syn::Attribute]) -> bool {
    fn requires_test(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(p) => p.is_ident("test"),
            syn::Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
                use syn::parse::Parser;
                let parser =
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
                let Ok(items) = parser.parse2(list.tokens.clone()) else {
                    return false;
                };
                if list.path.is_ident("all") {
                    items.iter().any(requires_test)
                } else {
                    !items.is_empty() && items.iter().all(requires_test)
                }
            }
            _ => false,
        }
    }
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Meta>().is_ok_and(|m| requires_test(&m))
    })
}
/// Expand only the visibility wrapper; other macro payloads remain opaque.
pub fn testkit_item(item: &syn::ItemMacro) -> syn::Result<Option<syn::Item>> {
    if test_only(&item.attrs)
        || item
            .mac
            .path
            .segments
            .last()
            .is_none_or(|s| s.ident != "testkit_visible")
    {
        return Ok(None);
    }
    syn::parse2::<TestkitVisible>(item.mac.tokens.clone()).map(|item| Some(item.0))
}
