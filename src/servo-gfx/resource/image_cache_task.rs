use image::base::{Image, load_from_memory, test_image_bin};
use resource::resource_task;
use resource::resource_task::ResourceTask;
use util::url::{make_url, UrlMap, url_map};

use clone_arc = std::arc::clone;
use core::pipes::{Chan, Port, SharedChan, stream};
use core::task::spawn;
use resource::util::spawn_listener;
use core::to_str::ToStr;
use core::util::replace;
use std::arc::ARC;
use std::net::url::Url;
use std::cell::Cell;

pub enum Msg {
    /// Tell the cache that we may need a particular image soon. Must be posted
    /// before Decode
    pub Prefetch(Url),

    // FIXME: We can probably get rid of this Cell now
    /// Used be the prefetch tasks to post back image binaries
    priv StorePrefetchedImageData(Url, Result<Cell<~[u8]>, ()>),

    /// Tell the cache to decode an image. Must be posted before GetImage/WaitForImage
    pub Decode(Url),

    /// Used by the decoder tasks to post decoded images back to the cache
    priv StoreImage(Url, Option<ARC<~Image>>),

    /// Request an Image object for a URL. If the image is not is not immediately
    /// available then ImageNotReady is returned.
    pub GetImage(Url, Chan<ImageResponseMsg>),

    /// Wait for an image to become available (or fail to load).
    pub WaitForImage(Url, Chan<ImageResponseMsg>),

    /// For testing
    priv OnMsg(fn~(msg: &Msg)),

    /// Clients must wait for a response before shutting down the ResourceTask
    pub Exit(Chan<()>)
}

pub enum ImageResponseMsg {
    ImageReady(ARC<~Image>),
    ImageNotReady,
    ImageFailed
}

impl ImageResponseMsg {
    pure fn clone() -> ImageResponseMsg {
        match &self {
          &ImageReady(ref img) => ImageReady(unsafe { clone_arc(img) }),
          &ImageNotReady => ImageNotReady,
          &ImageFailed => ImageFailed
        }
    }
}

impl ImageResponseMsg: cmp::Eq {
    pure fn eq(&self, other: &ImageResponseMsg) -> bool {
        // FIXME: Bad copies
        match (self.clone(), other.clone()) {
          (ImageReady(*), ImageReady(*)) => fail!(~"unimplemented comparison"),
          (ImageNotReady, ImageNotReady) => true,
          (ImageFailed, ImageFailed) => true,

          (ImageReady(*), _)
          | (ImageNotReady, _)
          | (ImageFailed, _) => false
        }
    }
    pure fn ne(&self, other: &ImageResponseMsg) -> bool {
        return !(*self).eq(other);
    }
}

pub type ImageCacheTask = SharedChan<Msg>;

type DecoderFactory = ~fn() -> ~fn(&[u8]) -> Option<Image>;

pub fn ImageCacheTask(resource_task: ResourceTask) -> ImageCacheTask {
    ImageCacheTask_(resource_task, default_decoder_factory)
}

pub fn ImageCacheTask_(resource_task: ResourceTask,
                       decoder_factory: DecoderFactory)
                    -> ImageCacheTask {
    // FIXME: Doing some dancing to avoid copying decoder_factory, our test
    // version of which contains an uncopyable type which rust will currently
    // copy unsoundly
    let decoder_factory_cell = Cell(move decoder_factory);

    let (port, chan) = stream();
    let chan = SharedChan(move chan);
    let port_cell = Cell(move port);
    let chan_cell = Cell(chan.clone());

    do spawn {
        ImageCache {
            resource_task: resource_task.clone(),
            decoder_factory: decoder_factory_cell.take(),
            port: port_cell.take(),
            chan: chan_cell.take(),
            state_map: url_map(),
            wait_map: url_map(),
            need_exit: None
        }.run();
    }

    move chan
}

fn SyncImageCacheTask(resource_task: ResourceTask) -> ImageCacheTask {
    let (port, chan) = stream();
    let port_cell = Cell(move port);

    do spawn {
        let port = port_cell.take();
        let inner_cache = ImageCacheTask(resource_task.clone());

        loop {
            let msg: Msg = port.recv();

            match move msg {
                GetImage(move url, move response) => {
                    inner_cache.send(WaitForImage(move url, move response));
                }
                Exit(move response) => {
                    inner_cache.send(Exit(move response));
                    break;
                }
                move msg => inner_cache.send(move msg)
            }
        }
    }

    return move SharedChan(move chan);
}

struct ImageCache {
    /// A handle to the resource task for fetching the image binaries
    resource_task: ResourceTask,
    /// Creates image decoders
    decoder_factory: DecoderFactory,
    /// The port on which we'll receive client requests
    port: Port<Msg>,
    /// A copy of the shared chan to give to child tasks
    chan: SharedChan<Msg>,
    /// The state of processsing an image for a URL
    state_map: UrlMap<ImageState>,
    /// List of clients waiting on a WaitForImage response
    wait_map: UrlMap<@mut ~[Chan<ImageResponseMsg>]>,
    mut need_exit: Option<Chan<()>>,
}

enum ImageState {
    Init,
    Prefetching(AfterPrefetch),
    Prefetched(@Cell<~[u8]>),
    Decoding,
    Decoded(@ARC<~Image>),
    Failed
}

enum AfterPrefetch {
    DoDecode,
    DoNotDecode
}

#[allow(non_implicitly_copyable_typarams)]
impl ImageCache {

    pub fn run() {

        let mut msg_handlers: ~[fn~(msg: &Msg)] = ~[];

        loop {
            let msg = self.port.recv();

            for msg_handlers.each |handler| {
                (*handler)(&msg)
            }

            debug!("image_cache_task: received: %?", msg);

            match move msg {
                Prefetch(move url) => self.prefetch(move url),
                StorePrefetchedImageData(move url, move data) => {
                    self.store_prefetched_image_data(move url, move data);
                }
                Decode(move url) => self.decode(move url),
                StoreImage(move url, move image) => self.store_image(move url, move image),
                GetImage(move url, move response) => self.get_image(move url, move response),
                WaitForImage(move url, move response) => {
                    self.wait_for_image(move url, move response)
                }
                OnMsg(move handler) => msg_handlers.push(handler),
                Exit(move response) => {
                    assert self.need_exit.is_none();
                    self.need_exit = Some(move response);
                }
            }

            let need_exit = replace(&mut self.need_exit, None);

            match move need_exit {
              Some(move response) => {
                // Wait until we have no outstanding requests and subtasks
                // before exiting
                let mut can_exit = true;
                for self.state_map.each_value |state| {
                    match *state {
                        Prefetching(*) => can_exit = false,
                        Decoding => can_exit = false,

                        Init | Prefetched(*) | Decoded(*) | Failed => ()
                    }
                }

                if can_exit {
                    response.send(());
                    break;
                } else {
                    self.need_exit = Some(move response);
                }
              }
              None => ()
            }
        }
    }

    priv fn get_state(url: Url) -> ImageState {
        match move self.state_map.find(&url) {
            Some(move state) => move state,
            None => Init
        }
    }

    priv fn set_state(url: Url, state: ImageState) {
        self.state_map.insert(move url, move state);
    }

    priv fn prefetch(url: Url) {
        match self.get_state(copy url) {
            Init => {
                let to_cache = self.chan.clone();
                let resource_task = self.resource_task.clone();
                let url_cell = Cell(copy url);

                do spawn {
                    let url = url_cell.take();
                    debug!("image_cache_task: started fetch for %s", url.to_str());

                    let image = load_image_data(copy url, resource_task.clone());

                    let result = if image.is_ok() {
                        Ok(Cell(result::unwrap(move image)))
                    } else {
                        Err(())
                    };
                    to_cache.send(StorePrefetchedImageData(copy url, move result));
                    debug!("image_cache_task: ended fetch for %s", (copy url).to_str());
                }

                self.set_state(move url, Prefetching(DoNotDecode));
            }

            Prefetching(*) | Prefetched(*) | Decoding | Decoded(*) | Failed => {
                // We've already begun working on this image
            }
        }
    }

    priv fn store_prefetched_image_data(url: Url, data: Result<Cell<~[u8]>, ()>) {
        match self.get_state(copy url) {
          Prefetching(next_step) => {
            match data {
              Ok(data_cell) => {
                let data = data_cell.take();
                self.set_state(copy url, Prefetched(@Cell(move data)));
                match next_step {
                  DoDecode => self.decode(move url),
                  _ => ()
                }
              }
              Err(*) => {
                self.set_state(copy url, Failed);
                self.purge_waiters(move url, || ImageFailed);
              }
            }
          }

          Init
          | Prefetched(*)
          | Decoding
          | Decoded(*)
          | Failed => {
            fail!(~"wrong state for storing prefetched image")
          }
        }
    }

    priv fn decode(url: Url) {
        match self.get_state(copy url) {
            Init => fail!(~"decoding image before prefetch"),

            Prefetching(DoNotDecode) => {
                // We don't have the data yet, queue up the decode
                self.set_state(move url, Prefetching(DoDecode))
            }

            Prefetching(DoDecode) => {
                // We don't have the data yet, but the decode request is queued up
            }

            Prefetched(data_cell) => {
                assert !data_cell.is_empty();

                let data = data_cell.take();
                let to_cache = self.chan.clone();
                let url_cell = Cell(copy url);
                let decode = (self.decoder_factory)();

                do spawn |move url_cell, move decode, move data, move to_cache| {
                    let url = url_cell.take();
                    debug!("image_cache_task: started image decode for %s", url.to_str());
                    let image = decode(data);
                    let image = if image.is_some() {
                        Some(ARC(~option::unwrap(move image)))
                    } else {
                        None
                    };
                    to_cache.send(StoreImage(copy url, move image));
                    debug!("image_cache_task: ended image decode for %s", url.to_str());
                }

                self.set_state(move url, Decoding);
            }

            Decoding | Decoded(*) | Failed => {
                // We've already begun decoding
            }
        }
    }

    priv fn store_image(url: Url, image: Option<ARC<~Image>>) {

        match self.get_state(copy url) {
          Decoding => {
            match image {
              Some(image) => {
                self.set_state(copy url, Decoded(@clone_arc(&image)));
                self.purge_waiters(move url, || ImageReady(clone_arc(&image)) );
              }
              None => {
                self.set_state(copy url, Failed);
                self.purge_waiters(move url, || ImageFailed );
              }
            }
          }

          Init
          | Prefetching(*)
          | Prefetched(*)
          | Decoded(*)
          | Failed => {
            fail!(~"incorrect state in store_image")
          }
        }

    }

    priv fn purge_waiters(url: Url, f: fn() -> ImageResponseMsg) {
          Some(waiters) => {
            let waiters = &mut *waiters;
            let mut new_waiters = ~[];
            new_waiters <-> *waiters;

            for new_waiters.each |response| {
                response.send(f());
            }

            *waiters <-> new_waiters;
            self.wait_map.remove(&url);
          }
          None => ()
        }
    }


    priv fn get_image(url: Url, response: Chan<ImageResponseMsg>) {
        match self.get_state(copy url) {
          Init => fail!(~"request for image before prefetch"),

          Prefetching(DoDecode) => {
            response.send(ImageNotReady);
          }

          Prefetching(DoNotDecode)
          | Prefetched(*) => fail!(~"request for image before decode"),

          Decoding => {
            response.send(ImageNotReady)
          }

          Decoded(image) => {
            response.send(ImageReady(clone_arc(image)));
          }

          Failed => {
            response.send(ImageFailed);
          }
        }
    }

    priv fn wait_for_image(url: Url, response: Chan<ImageResponseMsg>) {
        match self.get_state(copy url) {
            Init => fail!(~"request for image before prefetch"),

            Prefetching(DoNotDecode) | Prefetched(*) => fail!(~"request for image before decode"),

            Prefetching(DoDecode) | Decoding => {
                // We don't have this image yet
                match self.wait_map.find(&url) {
                    Some(waiters) => {
                        vec::push(&mut *waiters, move response);
                    }
                    None => {
                        self.wait_map.insert(move url, @mut ~[move response]);
                    }
                }
            }

            Decoded(image) => {
                response.send(ImageReady(clone_arc(image)));
            }

            Failed => {
                response.send(ImageFailed);
            }
        }
    }

}


trait ImageCacheTaskClient {
    fn exit();
}

impl ImageCacheTask: ImageCacheTaskClient {

    fn exit() {
        let (response_port, response_chan) = stream();
        self.send(Exit(move response_chan));
        response_port.recv();
    }

}

fn load_image_data(url: Url, resource_task: ResourceTask) -> Result<~[u8], ()> {
    let (response_port, response_chan) = stream();
    resource_task.send(resource_task::Load(move url, response_chan));

    let mut image_data = ~[];

    loop {
        match response_port.recv() {
            resource_task::Payload(data) => {
                image_data += data;
            }
            resource_task::Done(result::Ok(*)) => {
                return Ok(move image_data);
            }
            resource_task::Done(result::Err(*)) => {
                return Err(());
            }
        }
    }
}

fn default_decoder_factory() -> ~fn(&[u8]) -> Option<Image> {
    fn~(data: &[u8]) -> Option<Image> { load_from_memory(data) }
}

#[cfg(test)]
fn mock_resource_task(on_load: ~fn(resource: Chan<resource_task::ProgressMsg>)) -> ResourceTask {
    do spawn_listener |port: Port<resource_task::ControlMsg>, move on_load| {
        loop {
            match port.recv() {
              resource_task::Load(_, response) => {
                on_load(response);
              }
              resource_task::Exit => break
            }
        }
    }
}

#[test]
fn should_exit_on_request() {

    let mock_resource_task = mock_resource_task(|_response| () );

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let _url = make_url(~"file", None);

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
#[should_fail]
fn should_fail_if_unprefetched_image_is_requested() {

    let mock_resource_task = mock_resource_task(|_response| () );

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    let (chan, port) = stream();
    image_cache_task.send(GetImage(move url, move chan));
    port.recv();
}

#[test]
fn should_request_url_from_resource_task_on_prefetch() {
    let url_requested = Port();
    let url_requested_chan = url_requested.chan();

    let mock_resource_task = do mock_resource_task |response| {
        url_requested_chan.send(());
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(move url));
    url_requested.recv();
    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}


#[test]
#[should_fail]
fn should_fail_if_requesting_decode_of_an_unprefetched_image() {

    let mock_resource_task = mock_resource_task(|_response| () );

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Decode(move url));
    image_cache_task.exit();
}

#[test]
#[should_fail]
fn should_fail_if_requesting_image_before_requesting_decode() {

    let mock_resource_task = do mock_resource_task |response| {
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(copy url));
    // no decode message

    let (chan, _port) = stream();
    image_cache_task.send(GetImage(move url, move chan));

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_not_request_url_from_resource_task_on_multiple_prefetches() {
    let url_requested = comm::Port();
    let url_requested_chan = url_requested.chan();

    let mock_resource_task = do mock_resource_task |response| {
        url_requested_chan.send(());
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Prefetch(move url));
    url_requested.recv();
    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
    assert !url_requested.peek()
}

#[test]
fn should_return_image_not_ready_if_data_has_not_arrived() {

    let (wait_chan, wait_port) = pipes::stream();

    let mock_resource_task = do mock_resource_task |response, move wait_port| {
        // Don't send the data until after the client requests
        // the image
        wait_port.recv();
        response.send(resource_task::Payload(test_image_bin()));
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));
    let (response_chan, response_port) = stream();
    image_cache_task.send(GetImage(move url, move response_chan));
    assert response_port.recv() == ImageNotReady;
    wait_chan.send(());
    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_return_decoded_image_data_if_data_has_arrived() {

    let mock_resource_task = do mock_resource_task |response| {
        response.send(resource_task::Payload(test_image_bin()));
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    let wait_for_image = comm::Port();
    let wait_for_image_chan = wait_for_image.chan();

    image_cache_task.send(OnMsg(|msg| {
        match *msg {
          StoreImage(*) => wait_for_image_chan.send(()),
          _ => ()
        }
    }));

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    wait_for_image_chan.recv();

    let (response_chan, response_port) = stream();
    image_cache_task.send(GetImage(move url, move response_chan));
    match response_port.recv() {
      ImageReady(_) => (),
      _ => fail
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_return_decoded_image_data_for_multiple_requests() {

    let mock_resource_task = do mock_resource_task |response| {
        response.send(resource_task::Payload(test_image_bin()));
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    let wait_for_image = comm::Port();
    let wait_for_image_chan = wait_for_image.chan();

    image_cache_task.send(OnMsg(|msg| {
        match *msg {
          StoreImage(*) => wait_for_image_chan.send(()),
          _ => ()
        }
    }));

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    wait_for_image.recv();

    for iter::repeat(2) {
        let (response_chan, response_port) = stream();
        image_cache_task.send(GetImage(copy url, move response_chan));
        match response_port.recv() {
          ImageReady(_) => (),
          _ => fail
        }
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_not_request_image_from_resource_task_if_image_is_already_available() {

    let image_bin_sent = comm::Port();
    let image_bin_sent_chan = image_bin_sent.chan();

    let resource_task_exited = comm::Port();
    let resource_task_exited_chan = resource_task_exited.chan();

    let mock_resource_task = do spawn_listener |port: comm::Port<resource_task::ControlMsg>| {
        loop {
            match port.recv() {
                resource_task::Load(_, response) => {
                    response.send(resource_task::Payload(test_image_bin()));
                    response.send(resource_task::Done(result::Ok(())));
                    image_bin_sent_chan.send(());
                }
                resource_task::Exit => {
                    resource_task_exited_chan.send(());
                    break
                }
            }
        }
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    image_bin_sent.recv();

    image_cache_task.send(Prefetch(copy url));

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);

    resource_task_exited.recv();

    // Our resource task should not have received another request for the image
    // because it's already cached
    assert !image_bin_sent.peek();
}

#[test]
fn should_not_request_image_from_resource_task_if_image_fetch_already_failed() {

    let image_bin_sent = comm::Port();
    let image_bin_sent_chan = image_bin_sent.chan();

    let resource_task_exited = comm::Port();
    let resource_task_exited_chan = resource_task_exited.chan();

    let mock_resource_task = do spawn_listener |port: comm::Port<resource_task::ControlMsg>| {
        loop {
            match port.recv() {
                resource_task::Load(_, response) => {
                    response.send(resource_task::Payload(test_image_bin()));
                    response.send(resource_task::Done(result::Err(())));
                    image_bin_sent_chan.send(());
                }
                resource_task::Exit => {
                    resource_task_exited_chan.send(());
                    break
                }
            }
        }
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    image_bin_sent.recv();

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);

    resource_task_exited.recv();

    // Our resource task should not have received another request for the image
    // because it's already cached
    assert !image_bin_sent.peek();
}

#[test]
fn should_return_failed_if_image_bin_cannot_be_fetched() {

    let mock_resource_task = do mock_resource_task |response| {
        response.send(resource_task::Payload(test_image_bin()));
        // ERROR fetching image
        response.send(resource_task::Done(result::Err(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    let wait_for_prefetech = comm::Port();
    let wait_for_prefetech_chan = wait_for_prefetech.chan();

    image_cache_task.send(OnMsg(|msg| {
        match *msg {
          StorePrefetchedImageData(*) => wait_for_prefetech_chan.send(()),
          _ => ()
        }
    }));

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    wait_for_prefetech.recv();

    let (response_chan, response_port) = stream();
    image_cache_task.send(GetImage(move url, move response_chan));
    match response_port.recv() {
      ImageFailed => (),
      _ => fail
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_return_failed_for_multiple_get_image_requests_if_image_bin_cannot_be_fetched() {

    let mock_resource_task = do mock_resource_task |response | {
        response.send(resource_task::Payload(test_image_bin()));
        // ERROR fetching image
        response.send(resource_task::Done(result::Err(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    let wait_for_prefetech = comm::Port();
    let wait_for_prefetech_chan = wait_for_prefetech.chan();

    image_cache_task.send(OnMsg(|msg| {
        match *msg {
          StorePrefetchedImageData(*) => wait_for_prefetech_chan.send(()),
          _ => ()
        }
    }));

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    wait_for_prefetech.recv();

    let (response_chan, response_port) = stream();
    image_cache_task.send(GetImage(copy url, move response_chan));
    match response_port.recv() {
      ImageFailed => (),
      _ => fail
    }

    // And ask again, we should get the same response
    let (response_chan, response_port) = stream();
    image_cache_task.send(GetImage(move url, move response_chan));
    match response_port.recv() {
      ImageFailed => (),
      _ => fail
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_return_not_ready_if_image_is_still_decoding() {

    let (wait_to_decode_chan, wait_to_decode_port) = pipes::stream();

    let mock_resource_task = do mock_resource_task |response| {
        response.send(resource_task::Payload(test_image_bin()));
        response.send(resource_task::Done(result::Ok(())));
    };

    let wait_to_decode_port_cell = Cell(move wait_to_decode_port);
    let decoder_factory = fn~(move wait_to_decode_port_cell) -> ~fn(&[u8]) -> Option<Image> {
        let wait_to_decode_port = wait_to_decode_port_cell.take();
        fn~(data: &[u8], move wait_to_decode_port) -> Option<Image> {
            // Don't decode until after the client requests the image
            wait_to_decode_port.recv();
            load_from_memory(data)
        }
    };

    let image_cache_task = ImageCacheTask_(mock_resource_task, move decoder_factory);
    let url = make_url(~"file", None);

    let wait_for_prefetech = comm::Port();
    let wait_for_prefetech_chan = wait_for_prefetech.chan();

    image_cache_task.send(OnMsg(|msg| {
        match *msg {
          StorePrefetchedImageData(*) => wait_for_prefetech_chan.send(()),
          _ => ()
        }
    }));

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    wait_for_prefetech.recv();

    // Make the request
    let (response_chan, response_port) = stream();
    image_cache_task.send(GetImage(move url, move response_chan));

    match response_port.recv() {
      ImageNotReady => (),
      _ => fail
    }

    // Now decode
    wait_to_decode_chan.send(());

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_return_failed_if_image_decode_fails() {

    let mock_resource_task = do mock_resource_task |response| {
        // Bogus data
        response.send(resource_task::Payload(~[]));
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    let wait_for_decode = comm::Port();
    let wait_for_decode_chan = wait_for_decode.chan();

    image_cache_task.send(OnMsg(|msg| {
        match *msg {
          StoreImage(*) => wait_for_decode_chan.send(()),
          _ => ()
        }
    }));

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    wait_for_decode.recv();

    // Make the request
    let (response_chan, response_port) = stream();
    image_cache_task.send(GetImage(move url, move response_chan));

    match response_port.recv() {
      ImageFailed => (),
      _ => fail
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_return_image_on_wait_if_image_is_already_loaded() {

    let mock_resource_task = do mock_resource_task |response| {
        response.send(resource_task::Payload(test_image_bin()));
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    let wait_for_decode = comm::Port();
    let wait_for_decode_chan = wait_for_decode.chan();

    image_cache_task.send(OnMsg(|msg| {
        match *msg {
          StoreImage(*) => wait_for_decode_chan.send(()),
          _ => ()
        }
    }));

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    // Wait until our mock resource task has sent the image to the image cache
    wait_for_decode.recv();

    let (response_chan, response_port) = stream();
    image_cache_task.send(WaitForImage(move url, move response_chan));
    match response_port.recv() {
      ImageReady(*) => (),
      _ => fail
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_return_image_on_wait_if_image_is_not_yet_loaded() {

    let (wait_chan, wait_port) = pipes::stream();

    let mock_resource_task = do mock_resource_task |response, move wait_port| {
        wait_port.recv();
        response.send(resource_task::Payload(test_image_bin()));
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    let (response_chan, response_port) = stream();
    image_cache_task.send(WaitForImage(move url, move response_chan));

    wait_chan.send(());

    match response_port.recv() {
      ImageReady(*) => (),
      _ => fail
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn should_return_image_failed_on_wait_if_image_fails_to_load() {

    let (wait_chan, wait_port) = pipes::stream();

    let mock_resource_task = do mock_resource_task |response, move wait_port| {
        wait_port.recv();
        response.send(resource_task::Payload(test_image_bin()));
        response.send(resource_task::Done(result::Err(())));
    };

    let image_cache_task = ImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    let (response_chan, response_port) = stream();
    image_cache_task.send(WaitForImage(move url, move response_chan));

    wait_chan.send(());

    match response_port.recv() {
      ImageFailed => (),
      _ => fail
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}

#[test]
fn sync_cache_should_wait_for_images() {
    let mock_resource_task = do mock_resource_task |response| {
        response.send(resource_task::Payload(test_image_bin()));
        response.send(resource_task::Done(result::Ok(())));
    };

    let image_cache_task = SyncImageCacheTask(mock_resource_task);
    let url = make_url(~"file", None);

    image_cache_task.send(Prefetch(copy url));
    image_cache_task.send(Decode(copy url));

    let (response_chan, response_port) = stream();
    image_cache_task.send(GetImage(move url, move response_chan));
    match response_port.recv() {
      ImageReady(_) => (),
      _ => fail
    }

    image_cache_task.exit();
    mock_resource_task.send(resource_task::Exit);
}
