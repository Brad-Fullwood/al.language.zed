codeunit 50130 "Work Order Helper"
{
    /// <summary>Checks whether prechecks are currently enabled for the workspace.</summary>
    procedure Precheck(): Boolean
    begin
        exit(true);
    end;

    /// <summary>Runs the precheck validation for a single staging record.</summary>
    procedure PrecheckRecord(var Staging: Record "Work Order Staging"): Boolean
    begin
        exit(Staging.Amount > 0);
    end;

    /// <summary>Schedules the post of a work order staging record.</summary>
    procedure SchedulePost(var Staging: Record "Work Order Staging")
    begin
        Staging.Status := Staging.Status::Posting;
        Staging.Modify();
    end;

    procedure ProcessAllStaging()
    var
        Staging: Record "Work Order Staging";
    begin
        if Staging.FindSet(false) then
            repeat
                if this.PrecheckRecord(Staging) then
                    SchedulePost(Staging);
            until Staging.Next() = 0;
    end;

    procedure CountFields(): Integer
    var
        RecRef: RecordRef;
    begin
        RecRef.Open(Database::"Work Order Staging");
        exit(RecRef.FieldCount);
    end;

    local procedure InternalHelper(): Text
    begin
        exit('internal');
    end;
}
