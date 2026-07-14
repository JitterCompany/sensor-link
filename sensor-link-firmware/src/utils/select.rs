use futures::{
    future::{select, Either},
    pin_mut, Future,
};

pub enum Select2<A, B> {
    A(A),
    B(B),
}

pub enum Select3<A, B, C> {
    A(A),
    B(B),
    C(C),
}

impl<A, B, C> Into<Select3<A, B, C>> for Select2<A, B> {
    fn into(self) -> Select3<A, B, C> {
        match self {
            Select2::A(a) => Select3::A(a),
            Select2::B(b) => Select3::B(b),
        }
    }
}

/// Waits for either of the futures to complete.
/// First future is checked first (highest prio), last future last.
pub async fn select2<A, B>(a: A, b: B) -> Select2<A::Output, B::Output>
where
    A: Future,
    B: Future,
{
    pin_mut!(a, b);
    match select(a, b).await {
        Either::Left((a, _)) => Select2::A(a),
        Either::Right((b, _)) => Select2::B(b),
    }
}

/// Waits for either of the futures to complete.
/// First future is checked first (highest prio), last future last.
pub async fn select3<A, B, C>(a: A, b: B, c: C) -> Select3<A::Output, B::Output, C::Output>
where
    A: Future,
    B: Future,
    C: Future,
{
    pin_mut!(a, b, c);
    match select(a, select(b, c)).await {
        Either::Left((a, _)) => Select3::A(a),
        Either::Right((b_or_c, _)) => match b_or_c {
            Either::Left((b, _)) => Select3::B(b),
            Either::Right((c, _)) => Select3::C(c),
        },
    }
}
