extern crate iron;
extern crate octobot;

mod mocks;

use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;
use std::thread::{self, JoinHandle};

use iron::status;

use octobot::config::Config;
use octobot::repos::RepoConfig;
use octobot::users::UserConfig;
use octobot::github::*;
use octobot::github::api::Session;
use octobot::messenger::SlackMessenger;
use octobot::slack::SlackAttachmentBuilder;
use octobot::server::github_handler::GithubEventHandler;
use octobot::pr_merge::PRMergeMessage;

use mocks::mock_github::MockGithub;
use mocks::mock_slack::{SlackCall, MockSlack};

// this message gets appended only to review channel messages, not to slackbots
const REPO_MSG : &'static str = "(<http://the-github-host/some-user/some-repo|some-user/some-repo>)";

fn the_repo() -> Repo {
    Repo::parse("http://the-github-host/some-user/some-repo").unwrap()
}

struct GithubHandlerTest {
    handler: GithubEventHandler,
    github: Arc<MockGithub>,
    slack: Rc<MockSlack>,
    config: Arc<Config>,
    rx: Option<Receiver<PRMergeMessage>>,
    tx: Sender<PRMergeMessage>,
}

impl GithubHandlerTest {
    fn expect_slack_calls(&mut self, calls: Vec<SlackCall>) {
        self.handler.messenger = Box::new(SlackMessenger {
            config: self.config.clone(),
            slack: Rc::new(MockSlack::new(calls)),
        });
    }

    fn expect_will_merge_branches(&mut self, branches: Vec<String>) -> JoinHandle<()> {
        let timeout = Duration::from_millis(300);
        let rx = self.rx.take().unwrap();
        thread::spawn(move || {
            for branch in branches {
                let msg = rx.recv_timeout(timeout).expect(&format!("expected to recv msg for branch: {}", branch));
                match msg {
                    PRMergeMessage::Merge(req) => {
                        assert_eq!(branch, req.target_branch);
                    },
                    _ => {
                        panic!("Unexpected messages: {:?}", msg);
                    }
                };
           }

            let last_message = rx.recv_timeout(timeout);
            assert!(last_message.is_err());
        })
    }
}

fn new_test() -> GithubHandlerTest {
    let github = Arc::new(MockGithub::new());
    let slack = Rc::new(MockSlack::new(vec![]));
    let (tx, rx) = channel();

    let mut repos = RepoConfig::new();
    let mut data = HookBody::new();

    repos.insert(github.github_host(),
                 "some-user/some-repo",
                 "the-reviews-channel");
    data.repository = Repo::parse(&format!("http://{}/some-user/some-repo", github.github_host()))
        .unwrap();
    data.sender = User::new("joe-sender");

    let config = Arc::new(Config::new(UserConfig::new(), repos));

    GithubHandlerTest {
        github: github.clone(),
        slack: slack.clone(),
        config: config.clone(),
        rx: Some(rx),
        tx: tx.clone(),
        handler: GithubEventHandler {
            event: "ping".to_string(),
            data: data,
            action: "".to_string(),
            config: config.clone(),
            messenger: Box::new(SlackMessenger {
                config: config.clone(),
                slack: slack.clone(),
            }),
            github_session: github.clone(),
            pr_merge: tx.clone(),
        },
    }
}

fn some_pr() -> Option<PullRequest> {
    Some(PullRequest {
        title: "The PR".into(),
        number: 32,
        html_url: "http://the-pr".into(),
        state: "open".into(),
        user: User::new("the-pr-owner"),
        merged: None,
        merge_commit_sha: None,
        assignees: vec![User::new("assign1"), User::new("joe-reviewer")],
        head: BranchRef {
            ref_name: "pr-branch".into(),
            sha: "ffff0000".into(),
            user: User::new("some-user"),
            repo: the_repo(),
        },
        base: BranchRef {
            ref_name: "master".into(),
            sha: "1111eeee".into(),
            user: User::new("some-user"),
            repo: the_repo(),
        },
    })
}

#[test]
fn test_ping() {
    let mut test = new_test();
    test.handler.event = "ping".to_string();

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "ping".into()), resp);
}

#[test]
fn test_commit_comment_with_path() {
    let mut test = new_test();
    test.handler.event = "commit_comment".into();
    test.handler.action = "created".into();
    test.handler.data.comment = Some(Comment {
        commit_id: Some("abcdef00001111".into()),
        path: Some("src/main.rs".into()),
        body: Some("I think this file should change".into()),
        html_url: "http://the-comment".into(),
        user: User::new("joe-reviewer"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    test.expect_slack_calls(vec![
        SlackCall::new(
            "the-reviews-channel",
            &format!("Comment on \"src/main.rs\" (<http://the-github-host/some-user/some-repo/commit/abcdef00001111|abcdef0>) {}", REPO_MSG),
            vec![SlackAttachmentBuilder::new("I think this file should change")
                .title("joe.reviewer said:")
                .title_link("http://the-comment")
                .build()]
        )
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "commit_comment".into()), resp);
}

#[test]
fn test_commit_comment_no_path() {
    let mut test = new_test();
    test.handler.event = "commit_comment".into();
    test.handler.action = "created".into();
    test.handler.data.comment = Some(Comment {
        commit_id: Some("abcdef00001111".into()),
        path: None,
        body: Some("I think this file should change".into()),
        html_url: "http://the-comment".into(),
        user: User::new("joe-reviewer"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    test.expect_slack_calls(vec![
        SlackCall::new(
            "the-reviews-channel",
            &format!("Comment on \"abcdef0\" (<http://the-github-host/some-user/some-repo/commit/abcdef00001111|abcdef0>) {}", REPO_MSG),
            vec![SlackAttachmentBuilder::new("I think this file should change")
                .title("joe.reviewer said:")
                .title_link("http://the-comment")
                .build()]
        )
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "commit_comment".into()), resp);
}

#[test]
fn test_issue_comment() {
    let mut test = new_test();
    test.handler.event = "issue_comment".into();
    test.handler.action = "created".into();
    test.handler.data.issue = Some(Issue {
        title: "The Issue".into(),
        html_url: "http://the-issue".into(),
        user: User::new("the-pr-owner"),
        assignees: vec![User::new("assign1"), User::new("joe-reviewer")],
    });
    test.handler.data.comment = Some(Comment {
        commit_id: Some("abcdef00001111".into()),
        path: Some("src/main.rs".into()),
        body: Some("I think this file should change".into()),
        html_url: "http://the-comment".into(),
        user: User::new("joe-reviewer"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    let attach = vec![SlackAttachmentBuilder::new("I think this file should change")
                          .title("joe.reviewer said:")
                          .title_link("http://the-comment")
                          .build()];
    let msg = "Comment on \"<http://the-issue|The Issue>\"";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone())
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "issue_comment".into()), resp);
}

#[test]
fn test_pull_request_comment() {
    let mut test = new_test();
    test.handler.event = "pull_request_review_comment".into();
    test.handler.action = "created".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.comment = Some(Comment {
        commit_id: Some("abcdef00001111".into()),
        path: Some("src/main.rs".into()),
        body: Some("I think this file should change".into()),
        html_url: "http://the-comment".into(),
        user: User::new("joe-reviewer"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    let attach = vec![SlackAttachmentBuilder::new("I think this file should change")
                          .title("joe.reviewer said:")
                          .title_link("http://the-comment")
                          .build()];
    let msg = "Comment on \"<http://the-pr|The PR>\"";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone())
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr_review_comment".into()), resp);
}

#[test]
fn test_pull_request_review_commented() {
    let mut test = new_test();
    test.handler.event = "pull_request_review".into();
    test.handler.action = "submitted".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.review = Some(Review {
        state: "commented".into(),
        body: Some("I think this file should change".into()),
        html_url: "http://the-comment".into(),
        user: User::new("joe-reviewer"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    let attach = vec![SlackAttachmentBuilder::new("I think this file should change")
                          .title("joe.reviewer said:")
                          .title_link("http://the-comment")
                          .build()];
    let msg = "Comment on \"<http://the-pr|The PR>\"";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone())
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr_review [comment]".into()), resp);
}

#[test]
fn test_pull_request_comments_ignore_empty_messages() {
    let mut test = new_test();
    test.handler.event = "pull_request_review_comment".into();
    test.handler.action = "created".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.comment = Some(Comment {
        commit_id: Some("abcdef00001111".into()),
        path: Some("src/main.rs".into()),
        body: Some("".into()),
        html_url: "http://the-comment".into(),
        user: User::new("joe-reviewer"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    test.expect_slack_calls(vec![]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr_review_comment".into()), resp);
}

#[test]
fn test_pull_request_comments_ignore_octobot() {
    let mut test = new_test();
    test.handler.event = "pull_request_review_comment".into();
    test.handler.action = "created".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.comment = Some(Comment {
        commit_id: Some("abcdef00001111".into()),
        path: Some("src/main.rs".into()),
        body: Some("I think this file should change".into()),
        html_url: "http://the-comment".into(),
        user: User::new("octobot"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    test.expect_slack_calls(vec![]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr_review_comment".into()), resp);
}

#[test]
fn test_pull_request_review_approved() {
    let mut test = new_test();
    test.handler.event = "pull_request_review".into();
    test.handler.action = "submitted".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.review = Some(Review {
        state: "approved".into(),
        body: Some("I like it!".into()),
        html_url: "http://the-comment".into(),
        user: User::new("joe-reviewer"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    let attach = vec![SlackAttachmentBuilder::new("I like it!")
                          .title("Review: Approved")
                          .title_link("http://the-comment")
                          .color("good")
                          .build()];
    let msg = "joe.reviewer approved PR \"<http://the-pr|The PR>\"";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone())
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr_review".into()), resp);
}

#[test]
fn test_pull_request_review_changes_requested() {
    let mut test = new_test();
    test.handler.event = "pull_request_review".into();
    test.handler.action = "submitted".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.review = Some(Review {
        state: "changes_requested".into(),
        body: Some("It needs some work!".into()),
        html_url: "http://the-comment".into(),
        user: User::new("joe-reviewer"),
    });
    test.handler.data.sender = User::new("joe-reviewer");

    let attach = vec![SlackAttachmentBuilder::new("It needs some work!")
                          .title("Review: Changes Requested")
                          .title_link("http://the-comment")
                          .color("danger")
                          .build()];
    let msg = "joe.reviewer requested changes to PR \"<http://the-pr|The PR>\"";
    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone())
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr_review".into()), resp);
}

#[test]
fn test_pull_request_opened() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "opened".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.sender = User::new("the-pr-owner");

    let attach = vec![SlackAttachmentBuilder::new("")
                          .title("Pull Request #32: \"The PR\"")
                          .title_link("http://the-pr")
                          .build()];
    let msg = "Pull Request opened by the.pr.owner";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone())
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}

#[test]
fn test_pull_request_closed() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "closed".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.sender = User::new("the-pr-closer");

    let attach = vec![SlackAttachmentBuilder::new("")
                          .title("Pull Request #32: \"The PR\"")
                          .title_link("http://the-pr")
                          .build()];
    let msg = "Pull Request closed";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}

#[test]
fn test_pull_request_reopened() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "reopened".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.sender = User::new("the-pr-closer");

    let attach = vec![SlackAttachmentBuilder::new("")
                          .title("Pull Request #32: \"The PR\"")
                          .title_link("http://the-pr")
                          .build()];
    let msg = "Pull Request reopened";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}

#[test]
fn test_pull_request_assigned() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "assigned".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.sender = User::new("the-pr-closer");

    let attach = vec![SlackAttachmentBuilder::new("")
                          .title("Pull Request #32: \"The PR\"")
                          .title_link("http://the-pr")
                          .build()];
    let msg = "Pull Request assigned to assign1, joe.reviewer";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}

#[test]
fn test_pull_request_unassigned() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "unassigned".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.sender = User::new("the-pr-closer");

    let attach = vec![SlackAttachmentBuilder::new("")
                          .title("Pull Request #32: \"The PR\"")
                          .title_link("http://the-pr")
                          .build()];
    let msg = "Pull Request unassigned";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}

#[test]
fn test_pull_request_other() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "some-other-action".into();
    test.handler.data.pull_request = some_pr();
    test.handler.data.sender = User::new("the-pr-closer");

    // should not do anything!

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}


#[test]
fn test_pull_request_labeled_not_merged() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "labeled".into();
    test.handler.data.pull_request = some_pr();
    if let Some(ref mut pr) = test.handler.data.pull_request {
        pr.merged = Some(false);
    }
    test.handler.data.sender = User::new("the-pr-owner");

    // labeled but not merged --> noop

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}

#[test]
fn test_pull_request_merged_error_getting_labels() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "closed".into();
    test.handler.data.pull_request = some_pr();
    if let Some(ref mut pr) = test.handler.data.pull_request {
        pr.merged = Some(true);
    }
    test.handler.data.sender = User::new("the-pr-merger");

    // mock error on labels
    test.github.mock_get_pull_request_labels("some-user", "some-repo", 32, Err("whooops.".into()));

    let msg1 = "Pull Request merged";
    let attach1 = vec![
        SlackAttachmentBuilder::new("")
          .title("Pull Request #32: \"The PR\"")
          .title_link("http://the-pr")
          .build()
    ];

    let msg2 = "Error getting Pull Request labels";
    let attach2 = vec![
        SlackAttachmentBuilder::new("whooops.")
            .color("danger")
            .build()
    ];

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg1, REPO_MSG), attach1.clone()),
        SlackCall::new("@the.pr.owner", msg1, attach1.clone()),
        SlackCall::new("@assign1", msg1, attach1.clone()),
        SlackCall::new("@joe.reviewer", msg1, attach1.clone()),

        SlackCall::new("the-reviews-channel", &format!("{} {}", msg2, REPO_MSG), attach2.clone()),
        SlackCall::new("@the.pr.owner", msg2, attach2.clone()),
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}

#[test]
fn test_pull_request_merged_no_labels() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "closed".into();
    test.handler.data.pull_request = some_pr();
    if let Some(ref mut pr) = test.handler.data.pull_request {
        pr.merged = Some(true);
    }
    test.handler.data.sender = User::new("the-pr-merger");

    let attach = vec![SlackAttachmentBuilder::new("")
                          .title("Pull Request #32: \"The PR\"")
                          .title_link("http://the-pr")
                          .build()];
    let msg = "Pull Request merged";

    // mock no labels
    test.github.mock_get_pull_request_labels("some-user", "some-repo", 32, Ok(vec![]));

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);
}

#[test]
fn test_pull_request_merged_backport_labels() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "closed".into();
    test.handler.data.pull_request = some_pr();
    if let Some(ref mut pr) = test.handler.data.pull_request {
        pr.merged = Some(true);
    }
    test.handler.data.sender = User::new("the-pr-merger");

    let attach = vec![SlackAttachmentBuilder::new("")
                          .title("Pull Request #32: \"The PR\"")
                          .title_link("http://the-pr")
                          .build()];
    let msg = "Pull Request merged";

    // mock some labels
    test.github.mock_get_pull_request_labels("some-user", "some-repo", 32, Ok(vec![
        Label::new("other"),
        Label::new("backport-1.0"),
        Label::new("BACKPORT-2.0"),
        Label::new("non-matching"),
    ]));

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    let expect_thread = test.expect_will_merge_branches(vec!["release/1.0".into(), "release/2.0".into()]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);

    expect_thread.join().unwrap();
}

#[test]
fn test_pull_request_merged_retroactively_labeled() {
    let mut test = new_test();
    test.handler.event = "pull_request".into();
    test.handler.action = "labeled".into();
    test.handler.data.pull_request = some_pr();
    if let Some(ref mut pr) = test.handler.data.pull_request {
        pr.merged = Some(true);
    }
    test.handler.data.label = Some(Label::new("backport-7.123"));
    test.handler.data.sender = User::new("the-pr-merger");

    let attach = vec![SlackAttachmentBuilder::new("")
                          .title("Pull Request #32: \"The PR\"")
                          .title_link("http://the-pr")
                          .build()];
    let msg = "Pull Request merged";

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    let expect_thread = test.expect_will_merge_branches(vec!["release/7.123".into()]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "pr".into()), resp);

    expect_thread.join().unwrap();
}

#[test]
fn test_push_no_pr() {
    let mut test = new_test();
    test.handler.event = "push".into();
    test.handler.data.ref_name = Some("refs/heads/some-branch".into());
    test.handler.data.before = Some("abcdef0000".into());
    test.handler.data.after = Some("1111abcdef".into());

    test.github.mock_get_pull_requests("some-user", "some-repo", Some("open".into()), Some("1111abcdef"), Ok(vec![]));

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "push".into()), resp);
}

#[test]
fn test_push_with_pr() {
    let mut test = new_test();
    test.handler.event = "push".into();
    test.handler.data.ref_name = Some("refs/heads/some-branch".into());
    test.handler.data.before = Some("abcdef0000".into());
    test.handler.data.after = Some("1111abcdef".into());

    test.handler.data.commits = Some(vec![
        Commit {
            id: "aaaaaa000000".into(),
            tree_id: "".into(),
            message: "add stuff".into(),
            url: "http://commit1".into(),
        },
        Commit {
            id: "1111abcdef".into(),
            tree_id: "".into(),
            message: "fix stuff".into(),
            url: "http://commit2".into(),
        },
    ]);

    let pr1 = some_pr().unwrap();
    let mut pr2 = pr1.clone();
    pr2.number = 99;
    pr2.assignees = vec![User::new("assign2")];

    test.github.mock_get_pull_requests("some-user", "some-repo", Some("open".into()), Some("1111abcdef"), Ok(vec![pr1, pr2]));

    let msg = "joe.sender pushed 2 commit(s) to branch some-branch";
    let attach_common = vec![
        SlackAttachmentBuilder::new("<http://commit1|aaaaaa0>: add stuff").build(),
        SlackAttachmentBuilder::new("<http://commit2|1111abc>: fix stuff").build(),
    ];

    let mut attach1 = vec![
        SlackAttachmentBuilder::new("")
            .title("Pull Request #32: \"The PR\"")
            .title_link("http://the-pr")
            .build(),
    ];
    attach1.append(&mut attach_common.clone());

    let mut attach2 = vec![
        SlackAttachmentBuilder::new("")
            .title("Pull Request #99: \"The PR\"")
            .title_link("http://the-pr")
            .build(),
    ];
    attach2.append(&mut attach_common.clone());

    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach1.clone()),
        SlackCall::new("@the.pr.owner", msg, attach1.clone()),
        SlackCall::new("@assign1", msg, attach1.clone()),
        SlackCall::new("@joe.reviewer", msg, attach1.clone()),

        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach2.clone()),
        SlackCall::new("@the.pr.owner", msg, attach2.clone()),
        SlackCall::new("@assign2", msg, attach2.clone()),
        SlackCall::new("@joe.reviewer", msg, attach2.clone()),
    ]);

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "push".into()), resp);
}

#[test]
fn test_push_force_notify() {
    let mut test = new_test();

    test.handler.event = "push".into();
    test.handler.data.ref_name = Some("refs/heads/some-branch".into());
    test.handler.data.before = Some("abcdef0000".into());
    test.handler.data.after = Some("1111abcdef".into());
    test.handler.data.forced = Some(true);
    test.handler.data.compare = Some("http://compare-url".into());

    let pr = some_pr().unwrap();
    test.github.mock_get_pull_requests("some-user", "some-repo", Some("open".into()), Some("1111abcdef"), Ok(vec![pr]));

    let msg = "joe.sender pushed 0 commit(s) to branch some-branch";
    let attach = vec![
        SlackAttachmentBuilder::new("")
            .title("Pull Request #32: \"The PR\"")
            .title_link("http://the-pr")
            .build(),
    ];
    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    test.github.mock_comment_pull_request("some-user", "some-repo", 32,
                                          "Force-push detected: before: abcdef0, after: 1111abc ([compare](http://compare-url))",
                                          Ok(()));

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "push".into()), resp);
}

#[test]
fn test_push_force_notify_wip() {
    let mut test = new_test();

    test.handler.event = "push".into();
    test.handler.data.ref_name = Some("refs/heads/some-branch".into());
    test.handler.data.before = Some("abcdef0000".into());
    test.handler.data.after = Some("1111abcdef".into());
    test.handler.data.forced = Some(true);

    let mut pr = some_pr().unwrap();
    pr.title = "WIP: Awesome new feature".into();
    test.github.mock_get_pull_requests("some-user", "some-repo", Some("open".into()), Some("1111abcdef"), Ok(vec![pr]));

    let msg = "joe.sender pushed 0 commit(s) to branch some-branch";
    let attach = vec![
        SlackAttachmentBuilder::new("")
            .title("Pull Request #32: \"WIP: Awesome new feature\"")
            .title_link("http://the-pr")
            .build(),
    ];
    test.expect_slack_calls(vec![
        SlackCall::new("the-reviews-channel", &format!("{} {}", msg, REPO_MSG), attach.clone()),
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    // no assertions: should not comment about force-push

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "push".into()), resp);
}

#[test]
fn test_push_force_notify_ignored() {
    let mut test = new_test();

    test.handler.event = "push".into();
    test.handler.data.ref_name = Some("refs/heads/some-branch".into());
    test.handler.data.before = Some("abcdef0000".into());
    test.handler.data.after = Some("1111abcdef".into());
    test.handler.data.forced = Some(true);

    // change the repo to an unconfigured one
    test.handler.data.repository =
        Repo::parse(&format!("http://{}/some-other-user/some-other-repo", test.github.github_host())).unwrap();

    let pr = some_pr().unwrap();
    test.github.mock_get_pull_requests("some-other-user", "some-other-repo", Some("open"), Some("1111abcdef"), Ok(vec![pr]));

    let msg = "joe.sender pushed 0 commit(s) to branch some-branch";
    let attach = vec![
        SlackAttachmentBuilder::new("")
            .title("Pull Request #32: \"The PR\"")
            .title_link("http://the-pr")
            .build(),
    ];
    test.expect_slack_calls(vec![
        SlackCall::new("@the.pr.owner", msg, attach.clone()),
        SlackCall::new("@assign1", msg, attach.clone()),
        SlackCall::new("@joe.reviewer", msg, attach.clone()),
    ]);

    // no assertions: should not comment about force-push

    let resp = test.handler.handle_event().unwrap();
    assert_eq!((status::Ok, "push".into()), resp);
}