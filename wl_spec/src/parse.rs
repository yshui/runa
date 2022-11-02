use std::io::{BufRead, BufReader, Read};

use quick_xml::{
    events::{attributes::Attributes, Event},
    reader::Reader,
};
use thiserror::Error;

use super::protocol::*;
#[derive(Error, Debug)]
pub enum Error {
    #[error("Ill-formed protocol file")]
    IllFormed,
    #[error("Protocol must have a name")]
    NoName,
    #[error("Invalid UTF-8: {0}")]
    FromUtf8(#[from] std::string::FromUtf8Error),
    #[error("Invalid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("Invalid number: {0}")]
    ParseInt(#[from] std::num::ParseIntError),
    #[error("Invalid XML")]
    Xml(#[from] quick_xml::Error),
    #[error("Unexpected token: `{0}`")]
    UnexpectedToken(String),
    #[error("Unexpected type: `{0}`")]
    UnexpectedType(String),
    #[error("Unexpected end tag, expected: `{expected}`, got: `{actual}`")]
    UnexpectedEndTag { expected: String, actual: String },
}
type Result<T, E = Error> = std::result::Result<T, E>;

fn extract_end_tag<R: BufRead>(reader: &mut Reader<R>, tag: &[u8]) -> Result<()> {
    match reader.read_event_into(&mut Vec::new())? {
        Event::End(bytes) =>
            if bytes.name().local_name().as_ref() != tag {
                return Err(Error::UnexpectedEndTag {
                    expected: String::from_utf8_lossy(tag).into_owned(),
                    actual:   String::from_utf8_lossy(bytes.name().local_name().as_ref())
                        .into_owned(),
                })
            },
        other => return Err(Error::UnexpectedToken(format!("{:?}", other))),
    }
    Ok(())
}
pub fn parse<S: Read>(stream: S) -> Result<Protocol> {
    let mut reader = Reader::from_reader(BufReader::new(stream));
    reader.trim_text(true).expand_empty_elements(true);
    // Skip first <?xml ... ?> event
    let _ = reader.read_event_into(&mut Vec::new());
    parse_protocol(reader)
}

fn parse_protocol<R: BufRead>(mut reader: Reader<R>) -> Result<Protocol> {
    let mut protocol = match reader.read_event_into(&mut Vec::new())? {
        Event::Start(bytes) => {
            assert!(
                bytes.local_name().as_ref() == b"protocol",
                "Missing protocol toplevel tag"
            );
            if let Some(attr) = bytes
                .attributes()
                .filter_map(|res| res.ok())
                .find(|attr| attr.key.local_name().as_ref() == b"name")
            {
                Protocol::new(String::from_utf8(attr.value.into_owned())?)
            } else {
                return Err(Error::NoName)
            }
        },
        other => return Err(Error::UnexpectedToken(format!("{:?}", other))),
    };

    loop {
        match reader.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(bytes)) => {
                match bytes.local_name().as_ref() {
                    b"copyright" => {
                        // parse the copyright
                        let copyright = match reader.read_event_into(&mut Vec::new()) {
                            Ok(Event::Text(copyright)) =>
                                copyright.unescape().ok().map(std::borrow::Cow::into_owned),
                            Ok(Event::CData(copyright)) =>
                                String::from_utf8(copyright.into_inner().into()).ok(),
                            Err(e) => return Err(e.into()),
                            _ => return Err(Error::IllFormed),
                        };

                        extract_end_tag(&mut reader, b"copyright")?;
                        protocol.copyright = copyright
                    },
                    b"interface" => {
                        protocol
                            .interfaces
                            .push(parse_interface(&mut reader, bytes.attributes())?);
                    },
                    b"description" => {
                        protocol.description =
                            Some(parse_description(&mut reader, bytes.attributes())?);
                    },
                    _ =>
                        return Err(Error::UnexpectedToken(String::from_utf8(
                            bytes.local_name().as_ref().to_owned(),
                        )?)),
                }
            },
            Ok(Event::End(bytes)) => {
                assert!(
                    bytes.local_name().as_ref() == b"protocol",
                    "Unexpected closing token `{}`",
                    String::from_utf8_lossy(bytes.local_name().as_ref())
                );
                break
            },
            // ignore comments
            Ok(Event::Comment(_)) => {},
            e => return Err(Error::UnexpectedToken(format!("{:?}", e))),
        }
    }

    Ok(protocol)
}

fn parse_interface<R: BufRead>(reader: &mut Reader<R>, attrs: Attributes) -> Result<Interface> {
    let mut interface = Interface::new();
    for attr in attrs.filter_map(|res| res.ok()) {
        match attr.key.local_name().as_ref() {
            b"name" => interface.name = String::from_utf8(attr.value.into_owned())?,
            b"version" => interface.version = std::str::from_utf8(attr.value.as_ref())?.parse()?,
            _ => {},
        }
    }

    loop {
        match reader.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(bytes)) => match bytes.local_name().as_ref() {
                b"description" =>
                    interface.description = Some(parse_description(reader, bytes.attributes())?),
                b"request" => interface.requests.push(parse_event_or_request(
                    reader,
                    bytes.attributes(),
                    b"request",
                )?),
                b"event" => interface.events.push(parse_event_or_request(
                    reader,
                    bytes.attributes(),
                    b"event",
                )?),
                b"enum" => interface
                    .enums
                    .push(parse_enum(reader, bytes.attributes())?),
                _ =>
                    return Err(Error::UnexpectedToken(String::from_utf8(
                        bytes.local_name().as_ref().to_owned(),
                    )?)),
            },
            Ok(Event::End(bytes)) if bytes.local_name().as_ref() == b"interface" => break,
            _ => {},
        }
    }

    Ok(interface)
}

fn parse_description<R: BufRead>(
    reader: &mut Reader<R>,
    attrs: Attributes,
) -> Result<(String, String)> {
    let mut summary = String::new();
    for attr in attrs.filter_map(|res| res.ok()) {
        if attr.key.local_name().as_ref() == b"summary" {
            summary = String::from_utf8_lossy(&attr.value)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
        }
    }

    let mut description = String::new();
    // Some protocols have comments inside their descriptions, so we need to parse
    // them in a loop and concatenate the parts into a single block of text
    loop {
        match reader.read_event_into(&mut Vec::new()) {
            Ok(Event::Text(bytes)) => {
                if !description.is_empty() {
                    description.push_str("\n\n");
                }
                description.push_str(&bytes.unescape().unwrap_or_default())
            },
            Ok(Event::End(bytes)) if bytes.local_name().as_ref() == b"description" => break,
            Ok(Event::Comment(_)) => {},
            Err(e) => return Err(e.into()),
            e => return Err(Error::UnexpectedToken(format!("{:?}", e))),
        }
    }

    Ok((summary, description))
}

fn parse_enum<R: BufRead>(reader: &mut Reader<R>, attrs: Attributes) -> Result<Enum> {
    let mut enu = Enum::new();
    for attr in attrs.filter_map(|res| res.ok()) {
        match attr.key.local_name().as_ref() {
            b"name" => enu.name = String::from_utf8(attr.value.into_owned())?,
            b"since" => enu.since = std::str::from_utf8(&attr.value)?.parse()?,
            b"bitfield" =>
                if &attr.value[..] == b"true" {
                    enu.bitfield = true
                },
            _ => {},
        }
    }

    loop {
        match reader.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(bytes)) => match bytes.local_name().as_ref() {
                b"description" =>
                    enu.description = Some(parse_description(reader, bytes.attributes())?),
                b"entry" => enu.entries.push(parse_entry(reader, bytes.attributes())?),
                _ =>
                    return Err(Error::UnexpectedToken(String::from_utf8(
                        bytes.local_name().as_ref().to_owned(),
                    )?)),
            },
            Ok(Event::End(bytes)) if bytes.local_name().as_ref() == b"enum" => break,
            _ => {},
        }
    }

    Ok(enu)
}

fn parse_event_or_request<R: BufRead>(
    reader: &mut Reader<R>,
    attrs: Attributes,
    tag: &[u8],
) -> Result<Message> {
    let mut message = Message::new();
    for attr in attrs.filter_map(|res| res.ok()) {
        match attr.key.local_name().as_ref() {
            b"name" => message.name = String::from_utf8(attr.value.into_owned())?,
            b"type" => message.typ = Some(parse_type(&attr.value)?),
            b"since" => message.since = std::str::from_utf8(&attr.value)?.parse()?,
            _ => {},
        }
    }

    loop {
        match reader.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(bytes)) => match bytes.local_name().as_ref() {
                b"description" =>
                    message.description = Some(parse_description(reader, bytes.attributes())?),
                b"arg" => {
                    let arg = parse_arg(reader, bytes.attributes())?;
                    if arg.typ == Type::NewId && arg.interface.is_none() {
                        // Push implicit parameter: interface and version
                        message.args.push(Arg {
                            name:        "interface".to_string(),
                            typ:         Type::String,
                            interface:   None,
                            summary:     Some("interface of the bound object".into()),
                            description: None,
                            allow_null:  false,
                            enum_:       None,
                        });
                        message.args.push(Arg {
                            name:        "version".to_string(),
                            typ:         Type::Uint,
                            interface:   None,
                            summary:     Some("interface version of the bound object".into()),
                            description: None,
                            allow_null:  false,
                            enum_:       None,
                        });
                    }
                    message.args.push(arg);
                },
                _ =>
                    return Err(Error::UnexpectedToken(String::from_utf8(
                        bytes.local_name().as_ref().to_owned(),
                    )?)),
            },
            Ok(Event::End(bytes)) if bytes.local_name().as_ref() == tag => break,
            _ => {},
        }
    }

    Ok(message)
}

fn parse_arg<R: BufRead>(reader: &mut Reader<R>, attrs: Attributes) -> Result<Arg> {
    let mut arg = Arg::new();
    for attr in attrs.filter_map(|res| res.ok()) {
        match attr.key.local_name().as_ref() {
            b"name" => arg.name = String::from_utf8(attr.value.into_owned())?,
            b"type" => arg.typ = parse_type(&attr.value)?,
            b"summary" =>
                arg.summary = Some(
                    String::from_utf8_lossy(&attr.value)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            b"interface" => arg.interface = Some(String::from_utf8(attr.value.into_owned())?),
            b"allow-null" =>
                if &*attr.value == b"true" {
                    arg.allow_null = true
                },
            b"enum" => arg.enum_ = Some(String::from_utf8(attr.value.into_owned())?),
            _ => {},
        }
    }

    loop {
        match reader.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(bytes)) => match bytes.local_name().as_ref() {
                b"description" =>
                    arg.description = Some(parse_description(reader, bytes.attributes())?),
                _ =>
                    return Err(Error::UnexpectedToken(String::from_utf8(
                        bytes.local_name().as_ref().to_owned(),
                    )?)),
            },
            Ok(Event::End(bytes)) if bytes.local_name().as_ref() == b"arg" => break,
            _ => {},
        }
    }

    Ok(arg)
}

fn parse_type(txt: &[u8]) -> Result<Type> {
    Ok(match txt {
        b"int" => Type::Int,
        b"uint" => Type::Uint,
        b"fixed" => Type::Fixed,
        b"string" => Type::String,
        b"object" => Type::Object,
        b"new_id" => Type::NewId,
        b"array" => Type::Array,
        b"fd" => Type::Fd,
        b"destructor" => Type::Destructor,
        e =>
            return Err(Error::UnexpectedType(
                String::from_utf8_lossy(e).into_owned(),
            )),
    })
}

fn parse_entry<R: BufRead>(reader: &mut Reader<R>, attrs: Attributes) -> Result<Entry> {
    let mut entry = Entry::new();
    for attr in attrs.filter_map(|res| res.ok()) {
        match attr.key.local_name().as_ref() {
            b"name" => entry.name = String::from_utf8(attr.value.into_owned())?,
            b"value" => {
                entry.value = if attr.value.starts_with(b"0x") {
                    u32::from_str_radix(std::str::from_utf8(&attr.value[2..])?, 16)?
                } else {
                    std::str::from_utf8(&attr.value)?.parse()?
                };
            },
            b"since" => entry.since = std::str::from_utf8(&attr.value)?.parse()?,
            b"summary" =>
                entry.summary = Some(
                    String::from_utf8_lossy(&attr.value)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            _ => {},
        }
    }

    loop {
        match reader.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(bytes)) => match bytes.local_name().as_ref() {
                b"description" =>
                    entry.description = Some(parse_description(reader, bytes.attributes())?),
                _ =>
                    return Err(Error::UnexpectedToken(
                        String::from_utf8_lossy(bytes.local_name().as_ref()).into_owned(),
                    )),
            },
            Ok(Event::End(bytes)) if bytes.local_name().as_ref() == b"entry" => break,
            _ => {},
        }
    }

    Ok(entry)
}
