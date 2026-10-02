use super::*;
use std::sync::Arc;
use tokio::sync::oneshot;

#[tokio::test]
async fn followers_share_success_and_failure_but_later_calls_run_again() {
    for outcome in [Ok(42), Err("upstream unavailable")] {
        let flights = Arc::new(SingleFlight::default());
        let (started, entered) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let leader_flights = flights.clone();
        let leader = tokio::spawn(async move {
            leader_flights
                .run("account:v1".into(), move || async move {
                    started.send(()).unwrap();
                    released.await.unwrap();
                    outcome
                })
                .await
        });
        entered.await.unwrap();
        let follower_flights = flights.clone();
        let follower = tokio::spawn(async move {
            follower_flights
                .run("account:v1".into(), || async {
                    panic!("duplicate work must not run")
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while flights.flights.lock().get("account:v1").unwrap().waiters != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        release.send(()).unwrap();
        assert_eq!(leader.await.unwrap(), outcome);
        assert_eq!(follower.await.unwrap(), outcome);
        assert!(flights.flights.lock().is_empty());
        assert_eq!(
            flights.run("account:v1".into(), || async { Ok(99) }).await,
            Ok(99)
        );
    }
}

#[tokio::test]
async fn cancelling_all_waiters_drops_the_work_and_its_permit() {
    let flights = Arc::new(SingleFlight::<()>::default());
    let limit = Arc::new(tokio::sync::Semaphore::new(1));
    let (started, entered) = oneshot::channel();
    let worker_flights = flights.clone();
    let worker_limit = limit.clone();
    let worker = tokio::spawn(async move {
        worker_flights
            .run("old-key".into(), move || async move {
                let _permit = worker_limit.acquire_owned().await.unwrap();
                started.send(()).unwrap();
                std::future::pending().await
            })
            .await;
    });
    entered.await.unwrap();
    worker.abort();
    let _ = worker.await;
    assert_eq!(limit.available_permits(), 1);
    assert!(flights.flights.lock().is_empty());
}

#[tokio::test]
async fn cancelling_leader_keeps_follower_alive_and_versions_do_not_join() {
    let flights = Arc::new(SingleFlight::<u32>::default());
    let (started, entered) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let one = flights.clone();
    let leader = tokio::spawn(async move {
        one.run("account:v1".into(), move || async move {
            started.send(()).unwrap();
            released.await.unwrap();
            1
        })
        .await
    });
    entered.await.unwrap();
    let two = flights.clone();
    let follower = tokio::spawn(async move {
        two.run("account:v1".into(), || async { panic!("must join") })
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while flights.flights.lock().get("account:v1").unwrap().waiters != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    leader.abort();
    let _ = leader.await;
    assert_eq!(flights.run("account:v2".into(), || async { 2 }).await, 2);
    release.send(()).unwrap();
    assert_eq!(follower.await.unwrap(), 1);
}
