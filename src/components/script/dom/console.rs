

pub struct Console {
    script_chan: SharedChan<ScriptMsg>,
    script_context: *mut ScriptContext,
    wrapper: WrapperCache
}

pub impl Console {
    fn alert(&self, s: &str) {
        io::println(fmt!("LOG: %s", s));
    }

    pub fn new(script_chan: SharedChan<ScriptMsg>, script_context: *mut ScriptContext)
               -> @mut Window {
        let script_chan_copy = script_chan.clone();
        let console = @mut Console {
            wrapper: WrapperCache::new(),
            script_chan: script_chan,
            script_context: script_context,
        };

        unsafe {
            let compartment = (*script_context).js_compartment;
            console::create(compartment, console);
        }
        console
    }
}
