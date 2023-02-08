#[derive(Clone, Debug)]
pub struct Protocol {
    pub name:        String,
    pub copyright:   Option<String>,
    pub description: Option<(String, String)>,
    pub interfaces:  Vec<Interface>,
}

impl Protocol {
    pub fn new(name: String) -> Protocol {
        Protocol {
            name,
            copyright: None,
            description: None,
            interfaces: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Interface {
    pub name:        String,
    pub version:     u32,
    pub description: Option<(String, String)>,
    pub requests:    Vec<Message>,
    pub events:      Vec<Message>,
    pub enums:       Vec<Enum>,
}

impl Default for Interface {
    fn default() -> Interface {
        Interface {
            name:        String::new(),
            version:     1,
            description: None,
            requests:    Vec::new(),
            events:      Vec::new(),
            enums:       Vec::new(),
        }
    }
}

impl Interface {
    pub fn new() -> Interface {
        Interface::default()
    }
}

#[derive(Clone, Debug)]
pub struct Message {
    pub name:        String,
    pub typ:         Option<Type>,
    pub since:       u32,
    pub description: Option<(String, String)>,
    pub args:        Vec<Arg>,
}
impl Default for Message {
    fn default() -> Self {
        Message {
            name:        String::new(),
            typ:         None,
            since:       1,
            description: None,
            args:        Vec::new(),
        }
    }
}

impl Message {
    pub fn new() -> Message {
        Message::default()
    }

    pub fn all_null(&self) -> bool {
        self.args
            .iter()
            .all(|a| !((a.typ == Type::Object || a.typ == Type::NewId) && a.interface.is_some()))
    }
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub name:        String,
    pub typ:         Type,
    pub interface:   Option<String>,
    pub summary:     Option<String>,
    pub description: Option<(String, String)>,
    pub allow_null:  bool,
    pub enum_:       Option<String>,
}

impl Default for Arg {
    fn default() -> Arg {
        Arg {
            name:        String::new(),
            typ:         Type::Object,
            interface:   None,
            summary:     None,
            description: None,
            allow_null:  false,
            enum_:       None,
        }
    }
}

impl Arg {
    pub fn new() -> Arg {
        Arg::default()
    }
}

#[derive(Clone, Debug)]
pub struct Enum {
    pub name:        String,
    pub since:       u16,
    pub description: Option<(String, String)>,
    pub entries:     Vec<Entry>,
    pub bitfield:    bool,
}

impl Default for Enum {
    fn default() -> Enum {
        Enum {
            name:        String::new(),
            since:       1,
            description: None,
            entries:     Vec::new(),
            bitfield:    false,
        }
    }
}

impl Enum {
    pub fn new() -> Enum {
        Enum::default()
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name:        String,
    pub value:       u32,
    pub since:       u16,
    pub description: Option<(String, String)>,
    pub summary:     Option<String>,
}

impl Default for Entry {
    fn default() -> Entry {
        Entry {
            name:        String::new(),
            value:       0,
            since:       1,
            description: None,
            summary:     None,
        }
    }
}
impl Entry {
    pub fn new() -> Entry {
        Entry::default()
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum Type {
    Int,
    Uint,
    Fixed,
    String,
    Object,
    NewId,
    Array,
    Fd,
    Destructor,
}

impl Type {
    pub fn nullable(self) -> bool {
        matches!(self, Type::String | Type::Object)
    }
}
