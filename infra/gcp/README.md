# InkBridge Google Cloud deployment

This configuration is inert by default. `enable_deployment = false` defines
zero managed resources. Enabling it also requires the exact acknowledgement:

```hcl
enable_deployment          = true
deployment_acknowledgement = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
```

CI runs formatting, `init -backend=false`, validation, and credential-free
plans. It never runs `apply`, creates a project, links billing, or deploys a
resource.

## Why deployment is staged

Cloud Run requires an existing container image, while Artifact Registry and
Cloud Build must exist before that image can be built. The same Terraform
configuration therefore has two explicit broker stages:

1. **Bootstrap** — `cloud_run_image = ""`. Enables only the bootstrap APIs and
   creates the private versioned data bucket, a separate transient Cloud Build
   source bucket, Firestore database, runtime service account and IAM, and
   regional Artifact Registry repository. Cloud Run and Eventarc are
   deliberately absent.
2. **Runtime** — set `cloud_run_image` to the immutable Artifact Registry
   `@sha256:` URI produced by Cloud Build. Adds the private Cloud Run service,
   Eventarc service account/IAM, runtime APIs, and finalized-object trigger.

This breaks the image/repository cycle without deploying a placeholder image.

The Google Drive gateway is an independent, guarded Cloud Run Job layered on
the same bootstrap. Bootstrap creates only its service account and two empty
Secret Manager containers. Setting `drive_runtime_image` to an immutable digest
adds a private, manually invoked job in **dry-run** mode. The job receives no
`--apply` argument unless both `drive_runtime_apply_mode = true` and the exact
second acknowledgement are present:

```hcl
drive_runtime_apply_mode            = true
drive_runtime_apply_acknowledgement = "I_UNDERSTAND_DRIVE_APPLY_MUTATES_SYNC_STATE"
```

There is deliberately no Scheduler resource. A merged configuration therefore
cannot poll Drive by itself, and a dry-run job cannot upload evidence, mutate
broker state, create Drive files, or advance its page token.

## Remote state is mandatory for real applies

The partial `gcs` backend prevents a real deployment from silently relying on
fragile local state. Create one private, versioned state bucket before any real
plan or apply (this bootstrap bucket is intentionally outside the main state):

```text
gcloud storage buckets create gs://STATE_BUCKET \
  --project=PROJECT_ID \
  --location=REGION \
  --uniform-bucket-level-access \
  --public-access-prevention

gcloud storage buckets update gs://STATE_BUCKET --versioning

terraform init -reconfigure \
  -backend-config="bucket=STATE_BUCKET" \
  -backend-config="prefix=inkbridge/cloud-runtime"
```

Never commit `.tfvars`, state files, access tokens, service-account keys, or
saved plans. Copy `terraform.tfvars.example` to a private `.tfvars` file.

## Reviewed deployment sequence

1. Select an isolated project, billing account, region, and unique bucket
   names. Set a budget alert if desired; it is not a hard spending cap. Set
   `folder_transport_operator` to the `user:` or `group:` IAM member that will run the local
   adapter and operate the private conflict API. Runtime deployment refuses an empty operator so
   every preserved conflict has an authenticated resolution path.
2. Create and configure the private remote-state bucket.
3. Save and inspect the bootstrap plan. Apply only that exact saved plan.
4. Build a commit-tagged Linux amd64 image:

   ```text
   gcloud builds submit . \
     --project=PROJECT_ID \
     --region=REGION \
     --config=cloudbuild.runtime.yaml \
     --gcs-source-staging-dir=gs://BUILD_SOURCE_BUCKET/source \
     --service-account=projects/PROJECT_ID/serviceAccounts/inkbridge-builder@PROJECT_ID.iam.gserviceaccount.com \
     --substitutions=_IMAGE=REGION-docker.pkg.dev/PROJECT_ID/REPOSITORY/runtime:build-GIT_SHA
   ```

5. Resolve the pushed tag to its immutable digest:

   ```text
   gcloud artifacts docker images describe \
     REGION-docker.pkg.dev/PROJECT_ID/REPOSITORY/runtime:build-GIT_SHA \
     --format="value(image_summary.digest)"
   ```

6. Protect the selected digest from cleanup before deployment:

   ```text
   gcloud artifacts docker tags add \
     REGION-docker.pkg.dev/PROJECT_ID/REPOSITORY/runtime@sha256:DIGEST \
     REGION-docker.pkg.dev/PROJECT_ID/REPOSITORY/runtime:deployed-current
   ```

7. Set `cloud_run_image` to the same image path with the build tag replaced by
   `@sha256:...`. Save and inspect a fresh runtime plan, then apply only that
   exact saved plan.
8. Register an immutable original through the same private Eventarc path used
   for device updates. The metadata marker is required; unrelated objects in
   `Staging/` are ignored:

   ```text
   cargo run -p inkbridge-broker -- document-id ORIGINAL.pdf

   gcloud storage cp ORIGINAL.pdf gs://DEVICE_BUCKET/Staging/REGISTRATION_ID.pdf \
     --if-generation-match=0 \
     --custom-metadata=inkbridge-register-original=true,inkbridge-original-file-name=ORIGINAL.pdf
   ```

   Eventarc authenticates this finalized-object event with its existing private
   Cloud Run invoker identity. The broker validates the PDF and registers it
   idempotently. The local `document-id` command prints the stable ID to use in
   the two device folders. After registration, test one finalized update from
   each device folder.

After the reviewed apply, run the local folder transport through its dedicated
identity rather than through project-owner or broker credentials:

```text
gcloud config configurations create inkbridge-folder-transport
gcloud config set project PROJECT_ID --configuration=inkbridge-folder-transport
gcloud config set auth/impersonate_service_account \
  $(terraform output -raw folder_transport_service_account) \
  --configuration=inkbridge-folder-transport
```

Set `CLOUDSDK_ACTIVE_CONFIG_NAME=inkbridge-folder-transport` for the transport
process. The account can read broker outputs and preserved evidence, but IAM
permits it to create objects only under the two device-folder prefixes.

Use a separate authenticated gcloud configuration for the conflict API. Do not impersonate the
folder-transport service account for this path:

```text
gcloud config configurations create inkbridge-operator
gcloud config set project PROJECT_ID --configuration=inkbridge-operator
gcloud auth login --configuration=inkbridge-operator
gcloud run services proxy inkbridge-broker \
  --project=PROJECT_ID \
  --region=REGION \
  --port=8080 \
  --configuration=inkbridge-operator
```

Terraform grants the configured operator `roles/run.invoker`; it never grants anonymous
invocation. With the proxy running, inspect and resolve conflicts through
`http://localhost:8080/v1/documents/...`.

The Artifact Registry repository keeps the five most recent versions, retains
every `deployed-` tagged digest, and deletes older `build-` tagged or untagged
versions after seven days. Move `deployed-current` to the new digest only when
the matching runtime apply is ready. The dedicated build-source bucket deletes
source archives after one day, and the builder can read that bucket but not the
device-data bucket. The data bucket deletes live `Staging/` objects after one
day and their archived generations one day later. `BrokerOutbox/` is excluded
from age-based lifecycle rules because a pending Firestore commit may require
its exact generation indefinitely; the runtime deletes each output payload by
generation only after that commit is finalized. Cleanup failure may leak a
delivered payload but cannot break recovery. Originals, conflicts, and device
generations that may still be revision evidence are not automatically deleted.
Cloud Run remains IAM-private with no anonymous invoker, while accepting network ingress for the
configured operator. It uses zero minimum instances, one maximum instance, concurrency one, 2 vCPU,
8 GiB memory, and a 15-minute request timeout. Those resources are billed only while a request is
active because the service still scales to zero.

## Drive gateway rollout (not approved or applied)

The Drive job can be prepared after bootstrap without enabling the broker
Cloud Run service: it invokes the same broker core in-process and uses the
private bucket and Firestore directly. Its separate reviewed sequence is:

1. Add the OAuth client JSON and owner refresh token as Secret Manager
   **versions** outside Terraform. Terraform manages only the empty secret
   containers and access policy, so secret bytes never enter a plan or state.
2. Build a commit-tagged Linux amd64 image with
   `cloudbuild.drive-runtime.yaml`, resolve the tag to its digest, and protect
   the selected digest with a `deployed-` tag just as for the broker image.
3. Put the exact BOOX and Supernote Drive folder IDs, one `user:` or `group:`
   operator, and that immutable digest in a private tfvars file. Folder names
   are never used as authority.
4. Save and inspect a plan. Its `drive_runtime_stage` output must be `dry-run`,
   the job must have one task, no parallelism, zero retries, and no arguments.
5. Apply only that reviewed saved plan after a separate deployment approval.
6. Execute the private dry-run manually:

   ```text
   gcloud run jobs execute inkbridge-drive-gateway \
     --project=PROJECT_ID \
     --region=REGION \
     --wait
   ```

7. Inspect the execution and all Drive/GCS/Firestore evidence. Only after a
   successful disposable-file rehearsal should a later plan opt into `--apply`
   using the second acknowledgement above.
8. Keep execution manual through the repeated cross-device E2E gate. Add a
   conservative Scheduler and lease only in a later, separately reviewed PR.

Neither this repository configuration nor CI runs `terraform apply`, creates a
secret version, executes the job, or creates a recurring workload.

## Planned resources

The enabled stages manage:

- required project APIs (budget API only when a nonzero budget is requested);
- private regional Artifact Registry repository;
- private, short-lived Cloud Build source bucket;
- private versioned device-data bucket;
- Firestore Native database with deletion protection and point-in-time recovery;
- least-scope build, runtime, Eventarc, and local folder-transport service
  accounts/IAM, with device uploads barred from broker-owned namespaces;
- two empty Drive OAuth secret containers and a dedicated Drive gateway service
  account during bootstrap;
- an optional IAM-private, manually invoked Drive Cloud Run Job whose default
  template is non-mutating;
- IAM-private Cloud Run broker with explicit Eventarc and operator invokers; and
- Eventarc finalized-object trigger.
