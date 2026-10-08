//! 提供不依赖设备协议和领域 DTO 的集合读取原语。

use std::num::NonZeroUsize;

/// 一页读取完成后的后续游标和整套重启要求。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::adapters::device) struct PageState<Cursor> {
    next_cursor: Option<Cursor>,
    restart_required: bool,
}

impl<Cursor> PageState<Cursor> {
    /// 描述页面是否要求丢弃已收集内容并从初始游标重新读取。
    pub(in crate::adapters::device) const fn new(
        next_cursor: Option<Cursor>,
        restart_required: bool,
    ) -> Self {
        Self {
            next_cursor,
            restart_required,
        }
    }
}

/// 按游标读取全部页面；页面要求重启时丢弃当前序列，直到耗尽重启预算。
pub(in crate::adapters::device) fn read_restartable_pages<Page, Cursor, Error>(
    initial_cursor: Cursor,
    maximum_restarts: u8,
    mut read_page: impl FnMut(Cursor) -> Result<Page, Error>,
    page_state: impl Fn(&Page) -> PageState<Cursor>,
) -> Result<Vec<Page>, Error>
where
    Cursor: Copy,
{
    let mut restarts = 0_u8;
    'restart: loop {
        let mut pages = Vec::new();
        let mut cursor = initial_cursor;
        loop {
            let page = read_page(cursor)?;
            let state = page_state(&page);
            if state.restart_required && restarts < maximum_restarts {
                restarts += 1;
                continue 'restart;
            }
            pages.push(page);
            match state.next_cursor {
                Some(next_cursor) => cursor = next_cursor,
                None => return Ok(pages),
            }
        }
    }
}

/// 按固定非零批大小读取、校验并汇总结果，任一不完整批次立即返回调用方错误。
pub(in crate::adapters::device) fn read_complete_batches<Input, Batch, Output, Error>(
    inputs: &[Input],
    batch_size: NonZeroUsize,
    mut read_batch: impl FnMut(&[Input]) -> Result<Batch, Error>,
    validate_batch: impl Fn(&Batch, &[Input]) -> Result<(), Error>,
    is_complete: impl Fn(&Batch) -> bool,
    incomplete_error: impl Fn(&Batch) -> Error,
    into_outputs: impl Fn(Batch) -> Vec<Output>,
) -> Result<Vec<Output>, Error> {
    let mut outputs = Vec::with_capacity(inputs.len());
    for batch_inputs in inputs.chunks(batch_size.get()) {
        let batch = read_batch(batch_inputs)?;
        validate_batch(&batch, batch_inputs)?;
        if !is_complete(&batch) {
            return Err(incomplete_error(&batch));
        }
        outputs.extend(into_outputs(batch));
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::num::NonZeroUsize;

    use super::{PageState, read_complete_batches, read_restartable_pages};

    #[derive(Debug, Eq, PartialEq)]
    struct Page {
        label: &'static str,
        next: Option<u32>,
        restart: bool,
    }

    #[test]
    fn discards_the_partial_sequence_and_restarts_from_the_initial_cursor() {
        let mut calls = Vec::new();
        let mut responses = VecDeque::from([
            Page {
                label: "old-first",
                next: Some(1),
                restart: false,
            },
            Page {
                label: "old-incomplete",
                next: None,
                restart: true,
            },
            Page {
                label: "new-first",
                next: Some(1),
                restart: false,
            },
            Page {
                label: "new-last",
                next: None,
                restart: false,
            },
        ]);

        let pages = read_restartable_pages(
            0,
            2,
            |cursor| {
                calls.push(cursor);
                Ok::<_, &'static str>(responses.pop_front().expect("测试响应必须完整"))
            },
            |page| PageState::new(page.next, page.restart),
        )
        .unwrap();

        assert_eq!(calls, [0, 1, 0, 1]);
        assert_eq!(
            pages.iter().map(|page| page.label).collect::<Vec<_>>(),
            ["new-first", "new-last"]
        );
    }

    #[test]
    fn returns_the_last_sequence_after_the_restart_budget_is_exhausted() {
        let mut attempts = 0_u8;

        let pages = read_restartable_pages(
            0,
            2,
            |_| {
                attempts += 1;
                Ok::<_, &'static str>(Page {
                    label: "incomplete",
                    next: None,
                    restart: true,
                })
            },
            |page| PageState::new(page.next, page.restart),
        )
        .unwrap();

        assert_eq!(attempts, 3);
        assert_eq!(pages.len(), 1);
        assert!(pages[0].restart);
    }

    #[test]
    fn propagates_page_read_errors_without_retrying_them() {
        let mut attempts = 0_u8;

        let error = read_restartable_pages(
            0_u32,
            2,
            |_| {
                attempts += 1;
                Err::<Page, _>("read failed")
            },
            |page| PageState::new(page.next, page.restart),
        )
        .unwrap_err();

        assert_eq!(error, "read failed");
        assert_eq!(attempts, 1);
    }

    #[derive(Debug)]
    struct Batch {
        outputs: Vec<u32>,
        complete: bool,
    }

    #[test]
    fn reads_validates_and_collects_batches_in_input_order() {
        let mut requests = Vec::new();

        let outputs = read_complete_batches(
            &[1_u32, 2, 3, 4, 5],
            NonZeroUsize::new(2).unwrap(),
            |inputs| {
                requests.push(inputs.to_vec());
                Ok::<_, &'static str>(Batch {
                    outputs: inputs.iter().map(|value| value * 10).collect(),
                    complete: true,
                })
            },
            |batch, inputs| {
                if batch.outputs.len() == inputs.len() {
                    Ok(())
                } else {
                    Err("invalid batch")
                }
            },
            |batch| batch.complete,
            |_| "incomplete batch",
            |batch| batch.outputs,
        )
        .unwrap();

        assert_eq!(requests, [vec![1, 2], vec![3, 4], vec![5]]);
        assert_eq!(outputs, [10, 20, 30, 40, 50]);
    }

    #[test]
    fn rejects_an_incomplete_batch_before_reading_later_inputs() {
        let mut requests = 0_u8;

        let error = read_complete_batches(
            &[1_u32, 2, 3],
            NonZeroUsize::new(1).unwrap(),
            |inputs| {
                requests += 1;
                Ok::<_, &'static str>(Batch {
                    outputs: inputs.to_vec(),
                    complete: false,
                })
            },
            |_, _| Ok(()),
            |batch| batch.complete,
            |_| "incomplete batch",
            |batch| batch.outputs,
        )
        .unwrap_err();

        assert_eq!(error, "incomplete batch");
        assert_eq!(requests, 1);
    }

    #[test]
    fn returns_validation_errors_before_testing_completeness() {
        let error = read_complete_batches(
            &[1_u32],
            NonZeroUsize::new(1).unwrap(),
            |inputs| {
                Ok::<_, &'static str>(Batch {
                    outputs: inputs.to_vec(),
                    complete: false,
                })
            },
            |_, _| Err("invalid batch"),
            |batch| batch.complete,
            |_| "incomplete batch",
            |batch| batch.outputs,
        )
        .unwrap_err();

        assert_eq!(error, "invalid batch");
    }
}
