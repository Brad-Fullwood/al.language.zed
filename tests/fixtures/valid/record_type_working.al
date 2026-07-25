// Test: Record type references (currently working - must stay working)
codeunit 50103 "Record Type Test"
{
    procedure GetCustomer(CustomerNo: Code[20]) Result: Record Customer
    begin
    end;
    
    procedure ProcessDocument(var SalesHeader: Record "Sales Header"; DocType: Option Quote,Order,Invoice)
    begin
    end;
}
