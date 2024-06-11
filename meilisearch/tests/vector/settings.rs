use meili_snap::{json_string, snapshot};

use crate::common::{GetAllDocumentsOptions, Server};
use crate::json;
use crate::vector::generate_default_user_provided_documents;

#[actix_rt::test]
async fn update_embedder() {
    let server = Server::new().await;
    let index = server.index("doggo");
    let (value, code) = server.set_features(json!({"vectorStore": true})).await;
    snapshot!(code, @"200 OK");
    snapshot!(value, @r###"
    {
      "vectorStore": true,
      "metrics": false,
      "logsRoute": false
    }
    "###);

    let (response, code) = index
        .update_settings(json!({
          "embedders": { "manual": {}},
        }))
        .await;
    snapshot!(code, @"202 Accepted");
    server.wait_task(response.uid()).await;

    let (response, code) = index
        .update_settings(json!({
          "embedders": {
              "manual": {
                  "source": "userProvided",
                  "dimensions": 2,
              }
          },
        }))
        .await;
    snapshot!(code, @"202 Accepted");

    let ret = server.wait_task(response.uid()).await;
    snapshot!(ret, @r###"
    {
      "uid": 1,
      "indexUid": "doggo",
      "status": "failed",
      "type": "settingsUpdate",
      "canceledBy": null,
      "details": {
        "embedders": {
          "manual": {
            "source": "userProvided",
            "dimensions": 2
          }
        }
      },
      "error": {
        "message": "`.embedders.manual`: Field `model` unavailable for source `userProvided` (only available for sources: `huggingFace`, `openAi`, `ollama`). Available fields: `source`, `dimensions`, `distribution`",
        "code": "invalid_settings_embedders",
        "type": "invalid_request",
        "link": "https://docs.meilisearch.com/errors#invalid_settings_embedders"
      },
      "duration": "[duration]",
      "enqueuedAt": "[date]",
      "startedAt": "[date]",
      "finishedAt": "[date]"
    }
    "###);
}

#[actix_rt::test]
async fn reset_embedder_documents() {
    let server = Server::new().await;
    let index = generate_default_user_provided_documents(&server).await;

    let (response, code) = index.delete_settings().await;
    snapshot!(code, @"202 Accepted");
    server.wait_task(response.uid()).await;

    // Make sure the documents are still present
    let (documents, _code) = index.get_all_documents(Default::default()).await;
    snapshot!(json_string!(documents), @r###"
    {
      "results": [
        {
          "id": 0,
          "name": "kefir"
        },
        {
          "id": 1,
          "name": "echo"
        },
        {
          "id": 2,
          "name": "billou"
        },
        {
          "id": 3,
          "name": "intel"
        },
        {
          "id": 4,
          "name": "max"
        }
      ],
      "offset": 0,
      "limit": 20,
      "total": 5
    }
    "###);

    // Make sure we are still able to retrieve their vectors
    let (documents, _code) = index
        .get_all_documents(GetAllDocumentsOptions { retrieve_vectors: true, ..Default::default() })
        .await;
    snapshot!(json_string!(documents), @r###"
    {
      "results": [
        {
          "id": 0,
          "name": "kefir",
          "_vectors": {}
        },
        {
          "id": 1,
          "name": "echo",
          "_vectors": {}
        },
        {
          "id": 2,
          "name": "billou",
          "_vectors": {}
        },
        {
          "id": 3,
          "name": "intel",
          "_vectors": {}
        },
        {
          "id": 4,
          "name": "max",
          "_vectors": {}
        }
      ],
      "offset": 0,
      "limit": 20,
      "total": 5
    }
    "###);

    // Make sure the arroy DB has been cleared
    let (documents, _code) = index.search_post(json!({ "vector": [1, 1, 1] })).await;
    snapshot!(json_string!(documents), @r###"
    {
      "message": "Cannot find embedder with name `default`.",
      "code": "invalid_embedder",
      "type": "invalid_request",
      "link": "https://docs.meilisearch.com/errors#invalid_embedder"
    }
    "###);
}
