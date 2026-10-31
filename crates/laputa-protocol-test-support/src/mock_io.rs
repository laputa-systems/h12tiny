use crate::timer::Timer;
use futures_io::{AsyncRead, AsyncWrite};
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

enum Action {
    Read(Vec<u8>),
    Write(Vec<u8>),
    Wait(Duration),
    ReadError(Option<io::Error>),
    WriteError(Option<io::Error>),
}

pub struct Builder {
    actions: VecDeque<Action>,
    name: String,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Self {
        Self {
            actions: VecDeque::new(),
            name: String::new(),
        }
    }

    pub fn read(&mut self, buf: &[u8]) -> &mut Self {
        self.actions.push_back(Action::Read(buf.to_vec()));
        self
    }

    pub fn read_error(&mut self, error: io::Error) -> &mut Self {
        self.actions.push_back(Action::ReadError(Some(error)));
        self
    }

    pub fn write(&mut self, buf: &[u8]) -> &mut Self {
        self.actions.push_back(Action::Write(buf.to_vec()));
        self
    }

    pub fn write_error(&mut self, error: io::Error) -> &mut Self {
        self.actions.push_back(Action::WriteError(Some(error)));
        self
    }

    pub fn wait(&mut self, duration: Duration) -> &mut Self {
        self.actions
            .push_back(Action::Wait(duration.max(Duration::from_millis(1))));
        self
    }

    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = name.into();
        self
    }

    pub fn build(&mut self) -> Mock {
        Mock {
            actions: std::mem::take(&mut self.actions),
            name: std::mem::take(&mut self.name),
            sleep: None,
            read_wait: None,
            write_wait: None,
            late: None,
        }
    }

    pub fn build_with_handle(&mut self) -> (Mock, Handle) {
        let late = Arc::new(Mutex::new(VecDeque::new()));
        let mock = Mock {
            actions: std::mem::take(&mut self.actions),
            name: std::mem::take(&mut self.name),
            sleep: None,
            read_wait: None,
            write_wait: None,
            late: Some(late.clone()),
        };
        (mock, Handle { late })
    }
}

pub struct Mock {
    actions: VecDeque<Action>,
    name: String,
    sleep: Option<Pin<Box<Timer>>>,
    read_wait: Option<Waker>,
    write_wait: Option<Waker>,
    late: Option<Arc<Mutex<VecDeque<Action>>>>,
}

pub struct Handle {
    late: Arc<Mutex<VecDeque<Action>>>,
}

impl Handle {
    pub fn read(&mut self, buf: &[u8]) {
        self.late.lock().unwrap().push_back(Action::Read(buf.to_vec()));
    }

    pub fn write(&mut self, buf: &[u8]) {
        self.late.lock().unwrap().push_back(Action::Write(buf.to_vec()));
    }
}

impl fmt::Debug for Mock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mock")
            .field("name", &self.name)
            .field("actions", &self.actions.len())
            .finish()
    }
}

impl Mock {
    fn absorb_late(&mut self) {
        if let Some(late) = &self.late {
            let mut late = late.lock().unwrap();
            while let Some(action) = late.pop_front() {
                self.actions.push_back(action);
            }
        }
    }

    fn flush_empty(&mut self) {
        self.actions.retain(|action| match action {
            Action::Read(data) | Action::Write(data) => !data.is_empty(),
            _ => true,
        });
    }

    fn poll_wait(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let duration = match self.actions.front() {
            Some(Action::Wait(duration)) => *duration,
            _ => return Poll::Ready(()),
        };

        let sleep = match self.sleep.as_mut() {
            Some(sleep) => sleep,
            None => {
                let sleep = Box::pin(Timer::after(duration));
                self.sleep.insert(sleep)
            }
        };

        match sleep.as_mut().poll(cx) {
            Poll::Ready(_) => {
                self.sleep = None;
                self.actions.pop_front();
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn wake_writer(&mut self) {
        if let Some(waker) = self.write_wait.take() {
            waker.wake();
        }
    }

    fn wake_reader(&mut self) {
        if let Some(waker) = self.read_wait.take() {
            waker.wake();
        }
    }

    fn unexpected(&self, operation: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected {operation} in mock script {}", self.name),
        )
    }
}

impl AsyncRead for Mock {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        self.absorb_late();
        loop {
            if self.poll_wait(cx).is_pending() {
                return Poll::Pending;
            }
            self.flush_empty();

            match self.actions.front_mut() {
                Some(Action::Read(data)) => {
                    let count = buf.len().min(data.len());
                    buf[..count].copy_from_slice(&data[..count]);
                    data.drain(..count);
                    if data.is_empty() {
                        self.actions.pop_front();
                    }
                    self.wake_writer();
                    return Poll::Ready(Ok(count));
                }
                Some(Action::ReadError(error)) => {
                    let error = error
                        .take()
                        .expect("a read error action is consumed once");
                    self.actions.pop_front();
                    self.wake_writer();
                    return Poll::Ready(Err(error));
                }
                Some(Action::Write(_)) | Some(Action::WriteError(_)) => {
                    self.read_wait = Some(cx.waker().clone());
                    return Poll::Pending;
                }
                None => {
                    if self.late.is_some() {
                        self.read_wait = Some(cx.waker().clone());
                        return Poll::Pending;
                    }
                    return Poll::Ready(Ok(0));
                }
                Some(Action::Wait(_)) => unreachable!("waits are consumed above"),
            }
        }
    }
}

impl AsyncWrite for Mock {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let name = self.name.clone();
        self.absorb_late();
        loop {
            if self.poll_wait(cx).is_pending() {
                return Poll::Pending;
            }
            self.flush_empty();

            let index = self.actions.iter().position(|action| {
                matches!(
                    action,
                    Action::Write(_) | Action::WriteError(_) | Action::Wait(_)
                )
            });

            match index {
                Some(index) => match &mut self.actions[index] {
                    Action::Write(expected) => {
                        let count = buf.len().min(expected.len());
                        assert_eq!(
                            &buf[..count],
                            &expected[..count],
                            "mock script {} wrote unexpected bytes",
                            name
                        );
                        expected.drain(..count);
                        if expected.is_empty() {
                            self.actions.remove(index);
                        }
                        self.wake_reader();
                        return Poll::Ready(Ok(count));
                    }
                    Action::WriteError(error) => {
                        let error = error
                            .take()
                            .expect("a write error action is consumed once");
                        self.actions.remove(index);
                        self.wake_reader();
                        return Poll::Ready(Err(error));
                    }
                    Action::Wait(_) => {
                        self.write_wait = Some(cx.waker().clone());
                        return Poll::Pending;
                    }
                    _ => unreachable!("the scan stops at writes and waits"),
                },
                None => return Poll::Ready(Err(self.unexpected("write"))),
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }

        for action in &self.actions {
            match action {
                Action::Read(data) | Action::Write(data) => assert!(
                    data.is_empty(),
                    "mock script {} left {} byte(s) unconsumed",
                    self.name,
                    data.len()
                ),
                _ => {}
            }
        }
    }
}
