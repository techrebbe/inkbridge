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

## Why deployment has two stages

Cloud Run requires an existing container image, while Artifact Registry and
Cloud Build must exist before that image can be built. The same Terraform
configuration therefore has two explicit enabled stages:

1. **Bootstrap** — `cloud_run_image = ""`. Enables only the bootstrap APIs and
   creates the private versioned data bucket, a separate transient Cloud Build
   source bucket, Firestore database, runtime service account and IAM, and
   regional Artifact Registry repository. Cloud Run and Eventarc are
   deliberately absent.
2. **Runtime** — set `cloud_run_image` to the immutable Artifact Registry
   `@sha256:` URI produced by Cloud Build. Adds the private Cloud Run service,
   Eventarc service account/IAM, runtime APIs, and finalized-object trigger.

This breaks the image/repository cycle without deploying a placeholder image.

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
   `folder_transport_operator` to the `user:` or `group:` IAM member that
   will run the local adapter; leaving it empty creates the restricted identity
   but grants nobody permission to impersonate it.
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
Cloud Run remains private, uses zero minimum instances, one maximum instance,
concurrency one, 2 vCPU, 8 GiB memory, and a 15-minute request timeout. Those
resources are billed only while a request is active because the service still
scales to zero.

## Planned resources

The enabled stages manage:

- required project APIs (budget API only when a nonzero budget is requested);
- private regional Artifact Registry repository;
- private, short-lived Cloud Build source bucket;
- private versioned device-data bucket;
- Firestore Native database with deletion protection and point-in-time recovery;
- least-scope build, runtime, Eventarc, and local folder-transport service
  accounts/IAM, with device uploads barred from broker-owned namespaces;
- private Cloud Run broker; and
- Eventarc finalized-object trigger.
