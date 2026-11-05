//! UPnP control messages: SOAP requests from TVs and our responses.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::Event;

/// An action call: `Browse` with its arguments (`ObjectID`, `BrowseFlag`...).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub action: String,
    pub args: HashMap<String, String>,
}

impl Call {
    pub fn arg(&self, name: &str) -> &str {
        self.args.get(name).map(String::as_str).unwrap_or("")
    }
}

/// Parse `<s:Envelope><s:Body><u:Action><Arg>value</Arg>...</u:Action></s:Body></s:Envelope>`.
/// Namespace prefixes are ignored. None if there is no action in the body.
pub fn parse(xml: &str) -> Option<Call> {
    let mut reader = Reader::from_str(xml);
    let mut depth_in_body: Option<usize> = None; // depth relative to <Body>
    let mut action: Option<String> = None;
    let mut args = HashMap::new();
    let mut current: Option<(String, String)> = None;

    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => {
                let name = AsRef::<str>::as_ref(&e.local_name()).to_owned();
                match depth_in_body {
                    None if name == "Body" => depth_in_body = Some(0),
                    None => {}
                    Some(d) => {
                        let d = d + 1;
                        depth_in_body = Some(d);
                        match d {
                            1 => action = Some(name),
                            2 => current = Some((name, String::new())),
                            _ => {}
                        }
                    }
                }
            }
            Event::Empty(e) => {
                let name = AsRef::<str>::as_ref(&e.local_name()).to_owned();
                match depth_in_body {
                    Some(0) => action = Some(name),
                    Some(1) => {
                        args.insert(name, String::new());
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let Some((_, value)) = current.as_mut() {
                    value.push_str(&t.xml10_content());
                }
            }
            Event::CData(c) => {
                if let Some((_, value)) = current.as_mut() {
                    value.push_str(&c);
                }
            }
            Event::GeneralRef(r) => {
                if let Some((_, value)) = current.as_mut() {
                    let ch = match r.resolve_char_ref() {
                        Ok(Some(c)) => Some(c),
                        _ => match &*r {
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "amp" => Some('&'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            _ => None,
                        },
                    };
                    value.extend(ch);
                }
            }
            Event::End(_) => match depth_in_body {
                Some(0) => break,
                Some(d) => {
                    if d == 2
                        && let Some((name, value)) = current.take()
                    {
                        args.insert(name, value.trim().to_owned());
                    }
                    depth_in_body = Some(d - 1);
                }
                None => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Some(Call { action: action?, args })
}

/// Escape text for XML content and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not allowed in XML 1.0.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

const ENVELOPE_START: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>"#;
const ENVELOPE_END: &str = "</s:Body></s:Envelope>";

/// `<u:{action}Response>` with the given output arguments (values are escaped here).
pub fn response(service: &str, action: &str, out: &[(&str, &str)]) -> String {
    let mut xml = format!("{ENVELOPE_START}<u:{action}Response xmlns:u=\"{service}\">");
    for (name, value) in out {
        xml.push_str(&format!("<{name}>{}</{name}>", escape(value)));
    }
    xml.push_str(&format!("</u:{action}Response>{ENVELOPE_END}"));
    xml
}

/// UPnP error: 401 invalid action, 402 invalid args, 701 no such object, 501 action failed.
pub fn fault(code: u16, description: &str) -> String {
    format!(
        "{ENVELOPE_START}<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail>\
         <UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode>\
         <errorDescription>{}</errorDescription></UPnPError></detail></s:Fault>{ENVELOPE_END}",
        escape(description)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a Samsung TV sends (trimmed).
    const BROWSE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/" xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
      <ObjectID>a:12</ObjectID>
      <BrowseFlag>BrowseDirectChildren</BrowseFlag>
      <Filter>dc:title,res,upnp:albumArtURI</Filter>
      <StartingIndex>0</StartingIndex>
      <RequestedCount>48</RequestedCount>
      <SortCriteria/>
    </u:Browse>
  </s:Body>
</s:Envelope>"#;

    #[test]
    fn parses_browse() {
        let call = parse(BROWSE).unwrap();
        assert_eq!(call.action, "Browse");
        assert_eq!(call.arg("ObjectID"), "a:12");
        assert_eq!(call.arg("BrowseFlag"), "BrowseDirectChildren");
        assert_eq!(call.arg("RequestedCount"), "48");
        assert_eq!(call.arg("SortCriteria"), "");
        assert_eq!(call.arg("Missing"), "");
    }

    #[test]
    fn parses_escaped_values_and_empty_actions() {
        let xml = r#"<s:Envelope xmlns:s="x"><s:Body><u:Browse xmlns:u="y"><ObjectID>d:1:Tom &amp; Jerry&#x2F;x</ObjectID></u:Browse></s:Body></s:Envelope>"#;
        assert_eq!(parse(xml).unwrap().arg("ObjectID"), "d:1:Tom & Jerry/x");
        let empty = r#"<s:Envelope xmlns:s="x"><s:Body><u:GetSystemUpdateID xmlns:u="y"/></s:Body></s:Envelope>"#;
        assert_eq!(parse(empty).unwrap().action, "GetSystemUpdateID");
        assert_eq!(parse("<x>no body</x>"), None);
        assert_eq!(parse("not xml <<<"), None);
    }

    #[test]
    fn builds_responses() {
        let xml = response("urn:schemas-upnp-org:service:ContentDirectory:1", "GetSystemUpdateID", &[("Id", "7")]);
        assert!(xml.contains("<u:GetSystemUpdateIDResponse xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\"><Id>7</Id>"));
        assert!(response("s", "Browse", &[("Result", "<DIDL-Lite/>")]).contains("<Result>&lt;DIDL-Lite/&gt;</Result>"));
        assert!(fault(701, "No such object").contains("<errorCode>701</errorCode>"));
        assert_eq!(escape("a<b>&\"'\u{1}"), "a&lt;b&gt;&amp;&quot;&apos;");
    }
}
