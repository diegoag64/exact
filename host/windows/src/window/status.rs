use super::*;

impl<D: DataSource + Default + 'static> App<D> {
    pub(super) fn service_status(&mut self) {
        if let (Some(status), Some(p)) = (&mut self.status, &mut self.presenter) {
            status.service(p, || self.started.elapsed().as_secs_f64() * 1000.);
        }
    }
}

pub(super) fn install<D: DataSource>(
    hwnd: isize,
    p: &mut Presenter<D>,
) -> Option<crate::status::Status> {
    match crate::status::Status::new(hwnd) {
        Ok(mut status) => {
            status.sync(p);
            Some(status)
        }
        Err(e) => {
            eprintln!("exact-windows: {e}");
            None
        }
    }
}

pub(super) fn deadline(status: &Option<crate::status::Status>, events: &ActiveEventLoop) {
    if let Some(due) = status.as_ref().and_then(crate::status::Status::retry_at) {
        if !matches!(events.control_flow(), ControlFlow::WaitUntil(t) if t <= due) {
            events.set_control_flow(ControlFlow::WaitUntil(due));
        }
    }
}
